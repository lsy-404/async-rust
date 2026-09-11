use std::{
    fs::{self, File},
    io::BufWriter,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU8, Ordering},
        mpsc::{self, Receiver, Sender, SyncSender, TryRecvError},
        Arc, Mutex as StdMutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    FromSample, Sample, SampleFormat, SizedSample,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::stt::{LiveTranscriber, SttManager};

const CHUNK_FRAMES: usize = 4096;
const QUEUE_CHUNKS: usize = 32;
const MAX_WAV_BYTES: u64 = 512 * 1024 * 1024;
const FAILURE_OVERFLOW: u8 = 1;
const FAILURE_DEVICE: u8 = 2;
const FAILURE_SAMPLES: u8 = 3;

/// Which physical path feeds the shared capture pipeline (queue, VAD, live
/// transcription). Wire format matches the frontend's `CaptureMode`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RecordingSource {
    Microphone,
    SystemAudio,
}

// Lowest macOS version cpal's Core Audio Process Tap loopback path supports (per its README).
const MIN_MACOS_MAJOR: u32 = 14;
const MIN_MACOS_MINOR: u32 = 6;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SystemAudioCapability {
    pub available: bool,
    // Stable machine code for the frontend to map through i18n; never shown raw.
    pub reason: Option<String>,
}

fn parse_macos_version(text: &str) -> Option<(u32, u32)> {
    let mut parts = text.trim().split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor))
}

fn macos_supports_system_audio(version: Option<(u32, u32)>) -> bool {
    matches!(version, Some(found) if found >= (MIN_MACOS_MAJOR, MIN_MACOS_MINOR))
}

#[cfg(target_os = "macos")]
fn detected_macos_version() -> Option<(u32, u32)> {
    let output = std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_macos_version(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(target_os = "macos")]
pub fn system_audio_capability() -> SystemAudioCapability {
    if macos_supports_system_audio(detected_macos_version()) {
        SystemAudioCapability {
            available: true,
            reason: None,
        }
    } else {
        SystemAudioCapability {
            available: false,
            reason: Some("unsupported-os".into()),
        }
    }
}

#[cfg(target_os = "windows")]
pub fn system_audio_capability() -> SystemAudioCapability {
    // WASAPI loopback works on any output device cpal can already open; no
    // version gate needed on Windows.
    SystemAudioCapability {
        available: true,
        reason: None,
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn system_audio_capability() -> SystemAudioCapability {
    SystemAudioCapability {
        available: false,
        reason: Some("unsupported-platform".into()),
    }
}

fn gate_for(capability: &SystemAudioCapability) -> Result<(), String> {
    if capability.available {
        return Ok(());
    }
    Err(match capability.reason.as_deref() {
        Some("unsupported-os") => format!(
            "系统音频录制需要 macOS {MIN_MACOS_MAJOR}.{MIN_MACOS_MINOR} 或更高版本，请更新系统后重试。"
        ),
        _ => "当前平台不支持系统音频录制。".into(),
    })
}

fn system_audio_gate() -> Result<(), String> {
    gate_for(&system_audio_capability())
}

enum Control {
    Stop,
    Cancel,
}
type WorkerResult = Result<Option<PathBuf>, String>;
type TranscriptCallback = Arc<dyn Fn(String) -> Result<(), String> + Send + Sync>;
type ErrorCallback = Arc<dyn Fn(String) + Send + Sync>;
type LevelCallback = Arc<dyn Fn(f32) + Send + Sync>;
const LEVEL_EMIT_INTERVAL: Duration = Duration::from_millis(75);

struct ActiveRecording {
    session_id: String,
    control: Sender<Control>,
    canceled: Arc<std::sync::atomic::AtomicBool>,
    callback_gate: Arc<StdMutex<()>>,
    worker: Option<JoinHandle<WorkerResult>>,
}
impl ActiveRecording {
    fn finish(mut self, command: Control) -> WorkerResult {
        if matches!(command, Control::Cancel) {
            let _gate = self.callback_gate.lock().map_err(|_| "录音状态不可用。")?;
            self.canceled.store(true, Ordering::Release);
        }
        let _ = self.control.send(command);
        self.worker
            .take()
            .ok_or("录音线程不可用。")?
            .join()
            .map_err(|_| "录音线程意外停止。")?
    }
}
impl Drop for ActiveRecording {
    fn drop(&mut self) {
        if let Ok(_gate) = self.callback_gate.lock() {
            self.canceled.store(true, Ordering::Release);
        }
        let _ = self.control.send(Control::Cancel);
        if let Some(worker) = self.worker.take() {
            if let Ok(Ok(Some(path))) = worker.join() {
                let _ = fs::remove_file(path);
            }
        }
    }
}

struct CompletedCapture(Option<PathBuf>);
impl CompletedCapture {
    fn claim(mut self) -> Result<PathBuf, String> {
        self.0.take().ok_or_else(|| "录音已取消。".into())
    }
}
impl Drop for CompletedCapture {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_file(path);
        }
    }
}

pub struct RecordingManager {
    active: Mutex<Option<ActiveRecording>>,
}
impl Default for RecordingManager {
    fn default() -> Self {
        Self::new()
    }
}
impl RecordingManager {
    pub fn new() -> Self {
        Self {
            active: Mutex::new(None),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn start(
        &self,
        session_id: &str,
        data_root: &Path,
        stt: &SttManager,
        source: RecordingSource,
        language: Option<String>,
        on_transcript: TranscriptCallback,
        on_error: ErrorCallback,
        on_level: LevelCallback,
    ) -> Result<(), String> {
        if session_id.trim().is_empty() {
            return Err("找不到录音会话。".into());
        }
        let mut current = self.active.lock().await;
        if current.is_some() {
            return Err("已有正在进行的录音，请先停止或取消。".into());
        }
        let path = data_root
            .join("recordings")
            .join(format!("recording-{}.wav", Uuid::new_v4()));
        let (control, commands) = mpsc::channel();
        let (ready, startup) = mpsc::sync_channel(1);
        let canceled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let callback_gate = Arc::new(StdMutex::new(()));
        let guarded_transcript =
            guarded_callback(canceled.clone(), callback_gate.clone(), on_transcript);
        let stt_root = stt.clone_root();
        let worker_canceled = canceled.clone();
        let worker_error = on_error.clone();
        let thread_name = match source {
            RecordingSource::Microphone => "microphone-writer",
            RecordingSource::SystemAudio => "system-audio-writer",
        };
        let worker = thread::Builder::new()
            .name(thread_name.into())
            .spawn(move || {
                let result = recording_worker(
                    path,
                    commands,
                    ready,
                    stt_root,
                    source,
                    language,
                    guarded_transcript,
                    worker_canceled,
                    on_level,
                );
                if let Err(error) = &result {
                    worker_error(error.clone());
                }
                result
            })
            .map_err(|e| e.to_string())?;
        let active = ActiveRecording {
            session_id: session_id.into(),
            control,
            canceled,
            callback_gate,
            worker: Some(worker),
        };
        tokio::task::spawn_blocking(move || startup.recv())
            .await
            .map_err(|e| e.to_string())?
            .map_err(|_| "无法启动麦克风采集线程。")??;
        *current = Some(active);
        Ok(())
    }

    pub async fn stop(&self, session_id: &str) -> Result<PathBuf, String> {
        let mut current = self.active.lock().await;
        let active = current.as_ref().ok_or("没有正在进行的录音。")?;
        if active.session_id != session_id {
            return Err("录音属于其他会话，请返回原会话停止录音。".into());
        }
        let active = current.take().ok_or("没有正在进行的录音。")?;
        tokio::task::spawn_blocking(move || active.finish(Control::Stop).map(CompletedCapture))
            .await
            .map_err(|e| e.to_string())??
            .claim()
    }

    pub async fn cancel(&self) -> Result<(), String> {
        let mut current = self.active.lock().await;
        if let Some(active) = current.take() {
            tokio::task::spawn_blocking(move || active.finish(Control::Cancel))
                .await
                .map_err(|e| e.to_string())??;
        }
        Ok(())
    }
}

fn guarded_callback(
    canceled: Arc<std::sync::atomic::AtomicBool>,
    callback_gate: Arc<StdMutex<()>>,
    callback: TranscriptCallback,
) -> TranscriptCallback {
    Arc::new(move |text| {
        let _gate = callback_gate.lock().map_err(|_| "录音状态不可用。")?;
        if canceled.load(Ordering::Acquire) {
            return Ok(());
        }
        callback(text)
    })
}
impl Drop for RecordingManager {
    fn drop(&mut self) {
        self.active.get_mut().take();
    }
}

struct WavCapture {
    temporary: PathBuf,
    destination: PathBuf,
    writer: Option<hound::WavWriter<BufWriter<File>>>,
    samples: u64,
    max_samples: u64,
}
impl WavCapture {
    fn create(destination: PathBuf, sample_rate: u32) -> Result<Self, String> {
        let parent = destination.parent().ok_or("录音路径无效。")?;
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let temporary = destination.with_extension("wav.part");
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        let mut capture = Self {
            temporary,
            destination,
            writer: None,
            samples: 0,
            max_samples: ((MAX_WAV_BYTES - 44) / 2).min(u64::from(sample_rate) * 2 * 60 * 60),
        };
        capture.writer = Some(
            hound::WavWriter::new(
                BufWriter::new(file),
                hound::WavSpec {
                    channels: 1,
                    sample_rate,
                    bits_per_sample: 16,
                    sample_format: hound::SampleFormat::Int,
                },
            )
            .map_err(|e| e.to_string())?,
        );
        Ok(capture)
    }
    fn write(&mut self, samples: &[i16]) -> Result<(), String> {
        if self.samples + samples.len() as u64 > self.max_samples {
            return Err("录音已达到 512 MiB 或两小时上限，请开始新录音。".into());
        }
        let writer = self.writer.as_mut().ok_or("录音文件已经关闭。")?;
        for sample in samples {
            writer
                .write_sample(*sample)
                .map_err(|e| format!("无法写入录音：{e}"))?;
        }
        self.samples += samples.len() as u64;
        Ok(())
    }
    fn finish(mut self) -> Result<PathBuf, String> {
        if self.samples == 0 {
            return Err("麦克风没有提供音频采样。".into());
        }
        self.writer
            .take()
            .ok_or("录音文件已经关闭。")?
            .finalize()
            .map_err(|e| e.to_string())?;
        fs::OpenOptions::new()
            .write(true)
            .open(&self.temporary)
            .and_then(|file| file.sync_all())
            .map_err(|e| e.to_string())?;
        fs::rename(&self.temporary, &self.destination).map_err(|e| e.to_string())?;
        Ok(self.destination.clone())
    }
}
impl Drop for WavCapture {
    fn drop(&mut self) {
        self.writer.take();
        let _ = fs::remove_file(&self.temporary);
    }
}

// Folds capture chunks to a normalised RMS level, rate-limited so the UI meter
// doesn't get flooded with an event for every 4096-sample chunk.
struct LevelMeter {
    last_emit: Option<std::time::Instant>,
}
impl LevelMeter {
    fn new() -> Self {
        Self { last_emit: None }
    }
    fn sample(&mut self, chunk: &[i16]) -> Option<f32> {
        if chunk.is_empty() {
            return None;
        }
        let now = std::time::Instant::now();
        if self
            .last_emit
            .is_some_and(|last| now.duration_since(last) < LEVEL_EMIT_INTERVAL)
        {
            return None;
        }
        self.last_emit = Some(now);
        let sum_squares: f64 = chunk
            .iter()
            .map(|sample| {
                let normalized = f64::from(*sample) / f64::from(i16::MAX);
                normalized * normalized
            })
            .sum();
        let rms = (sum_squares / chunk.len() as f64).sqrt();
        Some((rms as f32).clamp(0.0, 1.0))
    }
}

fn failure_message(failure: &AtomicU8) -> Result<(), String> {
    match failure.load(Ordering::Acquire) {
        0 => Ok(()),
        FAILURE_OVERFLOW => Err("录音缓冲区溢出，录音已停止；请关闭占用资源的程序后重试。".into()),
        FAILURE_DEVICE => {
            Err("麦克风采集发生错误，录音已停止；请检查设备连接和麦克风权限。".into())
        }
        _ => Err("麦克风返回了无效音频采样，录音已停止。".into()),
    }
}
fn enqueue<T: SizedSample>(
    input: &[T],
    channels: usize,
    sender: &SyncSender<Vec<i16>>,
    failure: &AtomicU8,
) where
    f32: FromSample<T>,
{
    if failure.load(Ordering::Relaxed) != 0 {
        return;
    }
    if channels == 0 || !input.len().is_multiple_of(channels) {
        failure.store(FAILURE_SAMPLES, Ordering::Release);
        return;
    }
    for chunk in input.chunks(CHUNK_FRAMES * channels) {
        let mut mono = Vec::with_capacity(chunk.len() / channels);
        for frame in chunk.chunks_exact(channels) {
            let sample = frame
                .iter()
                .map(|sample| f32::from_sample(*sample))
                .sum::<f32>()
                / channels as f32;
            if !sample.is_finite() {
                failure.store(FAILURE_SAMPLES, Ordering::Release);
                return;
            }
            mono.push(i16::from_sample(sample.clamp(-1.0, 1.0)));
        }
        if sender.try_send(mono).is_err() {
            failure.store(FAILURE_OVERFLOW, Ordering::Release);
            return;
        }
    }
}
fn build_capture_stream<T: SizedSample>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    sender: SyncSender<Vec<i16>>,
    failure: Arc<AtomicU8>,
) -> Result<cpal::Stream, cpal::Error>
where
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels);
    let device_failure = failure.clone();
    device.build_input_stream(
        config,
        move |input: &[T], _| enqueue(input, channels, &sender, &failure),
        move |_| {
            device_failure.store(FAILURE_DEVICE, Ordering::Release);
        },
        Some(Duration::from_secs(10)),
    )
}
fn stream_for<T: SizedSample>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    sender: SyncSender<Vec<i16>>,
    failure: Arc<AtomicU8>,
) -> Result<cpal::Stream, String>
where
    f32: FromSample<T>,
{
    build_capture_stream::<T>(device, config, sender, failure)
        .map_err(|e| format!("无法开启麦克风，请检查权限和设备：{e}"))
}
// Same capture path as `stream_for`, but on the loopback (output) device cpal
// transparently builds when the requested device doesn't support input; the
// permission-denied case gets a distinct, actionable message.
fn system_stream_for<T: SizedSample>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    sender: SyncSender<Vec<i16>>,
    failure: Arc<AtomicU8>,
) -> Result<cpal::Stream, String>
where
    f32: FromSample<T>,
{
    build_capture_stream::<T>(device, config, sender, failure).map_err(|e| {
        if e.kind() == cpal::ErrorKind::PermissionDenied {
            "系统音频权限被拒绝。请在系统设置的隐私与安全性中允许 Async 录制系统音频，然后重试。".into()
        } else {
            format!("无法开启系统音频采集，请检查权限和设备：{e}")
        }
    })
}

fn forward_transcript_audio(
    pending: &mut Vec<i16>,
    input: &[i16],
    sender: &SyncSender<Vec<i16>>,
    final_chunk: bool,
) -> Result<(), String> {
    pending.extend_from_slice(input);
    while pending.len() >= CHUNK_FRAMES {
        let rest = pending.split_off(CHUNK_FRAMES);
        let chunk = std::mem::replace(pending, rest);
        sender.try_send(chunk).map_err(|_| {
            String::from("本地转写队列溢出，录音已停止；请关闭占用资源的程序后重试。")
        })?;
    }
    if final_chunk && !pending.is_empty() {
        sender.try_send(std::mem::take(pending)).map_err(|_| {
            String::from("本地转写队列溢出，录音已停止；请关闭占用资源的程序后重试。")
        })?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn recording_worker(
    path: PathBuf,
    commands: Receiver<Control>,
    ready: SyncSender<Result<(), String>>,
    stt_root: PathBuf,
    source: RecordingSource,
    language: Option<String>,
    on_transcript: TranscriptCallback,
    canceled: Arc<std::sync::atomic::AtomicBool>,
    on_level: LevelCallback,
) -> WorkerResult {
    let initialize = || {
        let host = cpal::default_host();
        // System audio rides the same output device cpal already exposes: asking it
        // for an *input* stream (it has none) makes cpal transparently create the
        // loopback path (Core Audio Process Tap on macOS, WASAPI loopback on Windows).
        let (device, supported) = match source {
            RecordingSource::Microphone => {
                let device = host.default_input_device().ok_or("没有可用的麦克风。")?;
                let supported = device
                    .default_input_config()
                    .map_err(|e| format!("无法读取麦克风配置：{e}"))?;
                (device, supported)
            }
            RecordingSource::SystemAudio => {
                system_audio_gate()?;
                let device = host
                    .default_output_device()
                    .ok_or("没有可用的系统输出设备。")?;
                let supported = device
                    .default_output_config()
                    .map_err(|e| format!("无法读取系统音频配置：{e}"))?;
                (device, supported)
            }
        };
        let config = supported.config();
        if !(8_000..=192_000).contains(&config.sample_rate)
            || config.channels == 0
            || config.channels > 32
        {
            return Err(match source {
                RecordingSource::Microphone => "麦克风采样率或声道数不受支持。".to_string(),
                RecordingSource::SystemAudio => "系统音频采样率或声道数不受支持。".to_string(),
            });
        }
        let writer = WavCapture::create(path, config.sample_rate)?;
        let (sender, audio) = mpsc::sync_channel(QUEUE_CHUNKS);
        let failure = Arc::new(AtomicU8::new(0));
        let stream = match source {
            RecordingSource::Microphone => match supported.sample_format() {
                SampleFormat::F32 => stream_for::<f32>(&device, config, sender, failure.clone()),
                SampleFormat::I16 => stream_for::<i16>(&device, config, sender, failure.clone()),
                SampleFormat::U16 => stream_for::<u16>(&device, config, sender, failure.clone()),
                format => Err(format!(
                    "麦克风格式 {format:?} 不受支持，请选择 PCM16 或 Float32 输入。"
                )),
            },
            RecordingSource::SystemAudio => match supported.sample_format() {
                SampleFormat::F32 => {
                    system_stream_for::<f32>(&device, config, sender, failure.clone())
                }
                SampleFormat::I16 => {
                    system_stream_for::<i16>(&device, config, sender, failure.clone())
                }
                SampleFormat::U16 => {
                    system_stream_for::<u16>(&device, config, sender, failure.clone())
                }
                format => Err(format!(
                    "系统音频格式 {format:?} 不受支持，请选择 PCM16 或 Float32 输出。"
                )),
            },
        }?;
        let mut live =
            SttManager::from_root(stt_root).live_transcriber(config.sample_rate, language)?;
        let transcript_queue_chunks =
            ((config.sample_rate as usize * 8).div_ceil(CHUNK_FRAMES)).max(48);
        let (transcript_sender, transcript_audio) = mpsc::sync_channel(transcript_queue_chunks);
        let transcription_cancel = canceled.clone();
        let transcript_worker = thread::Builder::new()
            .name("local-live-transcriber".into())
            .spawn(move || {
                transcription_worker(
                    &mut live,
                    transcript_audio,
                    on_transcript,
                    transcription_cancel,
                )
            })
            .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| format!("无法启动麦克风：{e}"))?;
        Ok::<_, String>((
            stream,
            writer,
            audio,
            transcript_sender,
            transcript_worker,
            failure,
        ))
    };
    let (stream, mut writer, audio, transcript_sender, transcript_worker, failure) =
        match initialize() {
            Ok(value) => value,
            Err(error) => {
                let _ = ready.send(Err(error.clone()));
                return Err(error);
            }
        };
    if ready.send(Ok(())).is_err() {
        canceled.store(true, Ordering::Release);
        drop(stream);
        drop(transcript_sender);
        let _ = transcript_worker.join();
        return Ok(None);
    }
    let mut outcome: Result<bool, String> = Ok(false);
    let mut pending_transcript_audio = Vec::with_capacity(CHUNK_FRAMES);
    let mut level_meter = LevelMeter::new();
    loop {
        match commands.try_recv() {
            Ok(Control::Stop) => {
                outcome = Ok(true);
                break;
            }
            Ok(Control::Cancel) | Err(TryRecvError::Disconnected) => break,
            Err(TryRecvError::Empty) => {}
        }
        if let Err(error) = failure_message(&failure) {
            outcome = Err(error);
            break;
        }
        match audio.recv_timeout(Duration::from_millis(10)) {
            Ok(samples) => {
                // Folding to a level and rate-limiting happens here, on the
                // dedicated writer thread, never on the CPAL audio callback.
                if let Some(level) = level_meter.sample(&samples) {
                    on_level(level);
                }
                if let Err(error) = writer.write(&samples).and_then(|_| {
                    forward_transcript_audio(
                        &mut pending_transcript_audio,
                        &samples,
                        &transcript_sender,
                        false,
                    )
                }) {
                    outcome = Err(error);
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                outcome = Err("麦克风采集意外停止。".into());
                break;
            }
        }
    }
    drop(stream);
    let stop = matches!(outcome, Ok(true));
    if stop {
        if let Err(error) = failure_message(&failure) {
            outcome = Err(error);
        }
        if outcome.is_ok() {
            for samples in audio.try_iter() {
                if let Err(error) = writer.write(&samples).and_then(|_| {
                    forward_transcript_audio(
                        &mut pending_transcript_audio,
                        &samples,
                        &transcript_sender,
                        false,
                    )
                }) {
                    outcome = Err(error);
                    break;
                }
            }
            if outcome.is_ok() {
                if let Err(error) = forward_transcript_audio(
                    &mut pending_transcript_audio,
                    &[],
                    &transcript_sender,
                    true,
                ) {
                    outcome = Err(error);
                }
            }
        }
    }
    if !matches!(outcome, Ok(true)) {
        canceled.store(true, Ordering::Release);
    }
    drop(transcript_sender);
    let transcription = transcript_worker
        .join()
        .map_err(|_| "本地转写线程意外停止。")?;
    outcome?;
    transcription?;
    if !stop {
        return Ok(None);
    }
    writer.finish().map(Some)
}

fn transcription_worker(
    live: &mut LiveTranscriber,
    audio: Receiver<Vec<i16>>,
    on_transcript: TranscriptCallback,
    canceled: Arc<std::sync::atomic::AtomicBool>,
) -> Result<(), String> {
    for samples in audio {
        if canceled.load(Ordering::Acquire) {
            return Ok(());
        }
        for text in live.accept_i16(&samples)? {
            if canceled.load(Ordering::Acquire) {
                return Ok(());
            }
            on_transcript(text)?;
        }
    }
    if canceled.load(Ordering::Acquire) {
        return Ok(());
    }
    for text in live.finish()? {
        if canceled.load(Ordering::Acquire) {
            return Ok(());
        }
        on_transcript(text)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../test/recording.rs"]
mod tests;

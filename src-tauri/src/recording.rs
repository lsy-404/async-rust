use std::{
    fs::{self, File},
    io::BufWriter,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU8, Ordering},
        mpsc::{self, Receiver, Sender, SyncSender, TryRecvError},
        Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    FromSample, Sample, SampleFormat, SizedSample,
};
use tokio::sync::Mutex;
use uuid::Uuid;

const CHUNK_FRAMES: usize = 4096;
const QUEUE_CHUNKS: usize = 32;
const MAX_WAV_BYTES: u64 = 512 * 1024 * 1024;
const FAILURE_OVERFLOW: u8 = 1;
const FAILURE_DEVICE: u8 = 2;
const FAILURE_SAMPLES: u8 = 3;

enum Control {
    Stop,
    Cancel,
}
type WorkerResult = Result<Option<PathBuf>, String>;

struct ActiveRecording {
    session_id: String,
    control: Sender<Control>,
    worker: Option<JoinHandle<WorkerResult>>,
}
impl ActiveRecording {
    fn finish(mut self, command: Control) -> WorkerResult {
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

    pub async fn start(&self, session_id: &str, data_root: &Path) -> Result<(), String> {
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
        let worker = thread::Builder::new()
            .name("microphone-writer".into())
            .spawn(move || recording_worker(path, commands, ready))
            .map_err(|e| e.to_string())?;
        let active = ActiveRecording {
            session_id: session_id.into(),
            control,
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
fn stream_for<T: SizedSample>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    sender: SyncSender<Vec<i16>>,
    failure: Arc<AtomicU8>,
) -> Result<cpal::Stream, String>
where
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels);
    let device_failure = failure.clone();
    device
        .build_input_stream(
            config,
            move |input: &[T], _| enqueue(input, channels, &sender, &failure),
            move |_| {
                device_failure.store(FAILURE_DEVICE, Ordering::Release);
            },
            Some(Duration::from_secs(10)),
        )
        .map_err(|e| format!("无法开启麦克风，请检查权限和设备：{e}"))
}

fn recording_worker(
    path: PathBuf,
    commands: Receiver<Control>,
    ready: SyncSender<Result<(), String>>,
) -> WorkerResult {
    let initialize = || {
        let host = cpal::default_host();
        let device = host.default_input_device().ok_or("没有可用的麦克风。")?;
        let supported = device
            .default_input_config()
            .map_err(|e| format!("无法读取麦克风配置：{e}"))?;
        let config = supported.config();
        if !(8_000..=192_000).contains(&config.sample_rate)
            || config.channels == 0
            || config.channels > 32
        {
            return Err("麦克风采样率或声道数不受支持。".into());
        }
        let writer = WavCapture::create(path, config.sample_rate)?;
        let (sender, audio) = mpsc::sync_channel(QUEUE_CHUNKS);
        let failure = Arc::new(AtomicU8::new(0));
        let stream = match supported.sample_format() {
            SampleFormat::F32 => stream_for::<f32>(&device, config, sender, failure.clone()),
            SampleFormat::I16 => stream_for::<i16>(&device, config, sender, failure.clone()),
            SampleFormat::U16 => stream_for::<u16>(&device, config, sender, failure.clone()),
            format => Err(format!(
                "麦克风格式 {format:?} 不受支持，请选择 PCM16 或 Float32 输入。"
            )),
        }?;
        stream.play().map_err(|e| format!("无法启动麦克风：{e}"))?;
        Ok::<_, String>((stream, writer, audio, failure))
    };
    let (stream, mut writer, audio, failure) = match initialize() {
        Ok(value) => value,
        Err(error) => {
            let _ = ready.send(Err(error.clone()));
            return Err(error);
        }
    };
    if ready.send(Ok(())).is_err() {
        return Ok(None);
    }
    let stop = loop {
        match commands.try_recv() {
            Ok(Control::Stop) => break true,
            Ok(Control::Cancel) | Err(TryRecvError::Disconnected) => break false,
            Err(TryRecvError::Empty) => {}
        }
        failure_message(&failure)?;
        match audio.recv_timeout(Duration::from_millis(10)) {
            Ok(samples) => writer.write(&samples)?,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err("麦克风采集意外停止。".into()),
        }
    };
    drop(stream);
    if !stop {
        return Ok(None);
    }
    failure_message(&failure)?;
    for samples in audio.try_iter() {
        writer.write(&samples)?;
    }
    writer.finish().map(Some)
}

#[cfg(test)]
#[path = "../../test/recording.rs"]
mod tests;

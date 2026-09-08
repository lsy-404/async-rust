use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Cursor, Read, Write},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use bzip2::read::BzDecoder;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sherpa_onnx::{
    LinearResampler, OfflineModelConfig, OfflineRecognizer, OfflineRecognizerConfig,
    OfflineWhisperModelConfig, SileroVadModelConfig, VadModelConfig, VoiceActivityDetector,
};
use symphonia::core::{
    audio::SampleBuffer, codecs::DecoderOptions, errors::Error as AudioError,
    formats::FormatOptions, io::MediaSourceStream, meta::MetadataOptions, probe::Hint,
};
use tokio::{io::AsyncWriteExt, sync::Semaphore};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const MODEL_NAME: &str = "Whisper tiny multilingual (INT8)";
const SAMPLE_RATE: i32 = 16_000;
const VAD_WINDOW: usize = 512;
pub(crate) const MAX_AUDIO_BYTES: u64 = 512 * 1024 * 1024;
const MAX_SAMPLES: usize = SAMPLE_RATE as usize * 2 * 60 * 60;
const EXPECTED_FILES: [&str; 4] = [
    "tiny-encoder.int8.onnx",
    "tiny-decoder.int8.onnx",
    "tiny-tokens.txt",
    "silero_vad.onnx",
];

struct Asset {
    url: &'static str,
    bytes: u64,
    sha256: &'static str,
}
const WHISPER: Asset = Asset {
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-whisper-tiny.tar.bz2",
    bytes: 116_204_861,
    sha256: "c46116994e539aa165266d96b325252728429c12535eb9d8b6a2b10f129e66b1",
};
const VAD: Asset = Asset {
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx",
    bytes: 643_854,
    sha256: "9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6",
};
const DOWNLOAD_BYTES: u64 = WHISPER.bytes + VAD.bytes;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SttStatus {
    pub ready: bool,
    pub model_name: String,
    pub model_path: String,
    pub size_bytes: u64,
}
#[derive(Clone, Debug, Serialize)]
pub struct DownloadProgress {
    pub downloaded: u64,
    pub total: u64,
}
#[derive(Serialize, Deserialize)]
struct Receipt {
    whisper: String,
    vad: String,
    files: BTreeMap<String, FileReceipt>,
}
#[derive(Serialize, Deserialize)]
struct FileReceipt {
    bytes: u64,
    sha256: String,
}

pub struct SttManager {
    root: PathBuf,
    download: Mutex<Option<CancellationToken>>,
    inference: Arc<Semaphore>,
}

struct DownloadGuard<'a>(&'a Mutex<Option<CancellationToken>>);
impl Drop for DownloadGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.0.lock() {
            *active = None;
        }
    }
}
struct StagingGuard(PathBuf);
impl Drop for StagingGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn check_canceled(cancel: &CancellationToken) -> Result<(), String> {
    if cancel.is_cancelled() {
        Err("语音任务已取消。".into())
    } else {
        Ok(())
    }
}
fn regular_file(path: &Path) -> Result<fs::Metadata, String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_file() {
        return Err("模型文件不是普通文件。".into());
    }
    Ok(metadata)
}
fn file_receipt(path: &Path, cancel: &CancellationToken) -> Result<FileReceipt, String> {
    let size = regular_file(path)?.len();
    if size == 0 || size > WHISPER.bytes * 4 {
        return Err("模型文件大小无效。".into());
    }
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        check_canceled(cancel)?;
        let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(FileReceipt {
        bytes: size,
        sha256: format!("{:x}", hash.finalize()),
    })
}
fn verified_install(root: &Path, cancel: &CancellationToken) -> Result<(), String> {
    let receipt_path = root.join("receipt.json");
    if regular_file(&receipt_path)?.len() > 4096 {
        return Err("模型记录无效。".into());
    }
    let receipt: Receipt =
        serde_json::from_slice(&fs::read(receipt_path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    if receipt.whisper != WHISPER.sha256 || receipt.vad != VAD.sha256 || receipt.files.len() != 4 {
        return Err("模型版本不匹配，请重新下载。".into());
    }
    for name in EXPECTED_FILES {
        let expected = receipt.files.get(name).ok_or("模型文件不完整。")?;
        let actual = file_receipt(&root.join(name), cancel)?;
        if actual.bytes != expected.bytes || actual.sha256 != expected.sha256 {
            return Err("模型校验失败，请重新下载。".into());
        }
    }
    Ok(())
}

impl SttManager {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            download: Mutex::new(None),
            inference: Arc::new(Semaphore::new(1)),
        }
    }
    fn model_path(&self) -> PathBuf {
        self.root.join("whisper-tiny-multilingual-int8")
    }
    pub async fn status(&self) -> Result<SttStatus, String> {
        let root = self.model_path();
        tokio::task::spawn_blocking(move || SttStatus {
            ready: verified_install(&root, &CancellationToken::new()).is_ok(),
            model_name: MODEL_NAME.into(),
            model_path: root.to_string_lossy().into_owned(),
            size_bytes: DOWNLOAD_BYTES,
        })
        .await
        .map_err(|e| e.to_string())
    }
    pub fn cancel_download(&self) -> Result<(), String> {
        if let Some(cancel) = self
            .download
            .lock()
            .map_err(|_| "下载状态不可用。")?
            .as_ref()
        {
            cancel.cancel();
        }
        Ok(())
    }
    pub async fn install(
        &self,
        progress: impl Fn(DownloadProgress) + Send + Sync,
    ) -> Result<SttStatus, String> {
        let cancel = CancellationToken::new();
        {
            let mut download = self.download.lock().map_err(|_| "下载状态不可用。")?;
            if download.is_some() {
                return Err("模型正在下载。".into());
            }
            *download = Some(cancel.clone());
        }
        let _active = DownloadGuard(&self.download);
        if self.status().await?.ready {
            progress(DownloadProgress {
                downloaded: DOWNLOAD_BYTES,
                total: DOWNLOAD_BYTES,
            });
            return self.status().await;
        }
        tokio::fs::create_dir_all(&self.root)
            .await
            .map_err(|e| e.to_string())?;
        let staging = self.root.join(format!(".install-{}", Uuid::new_v4()));
        tokio::fs::create_dir(&staging)
            .await
            .map_err(|e| e.to_string())?;
        let cleanup = StagingGuard(staging.clone());
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .read_timeout(Duration::from_secs(60))
            .timeout(Duration::from_secs(30 * 60))
            .build()
            .map_err(|e| e.to_string())?;
        progress(DownloadProgress {
            downloaded: 0,
            total: DOWNLOAD_BYTES,
        });
        download_asset(
            &client,
            &WHISPER,
            &staging.join("whisper.part"),
            &cancel,
            0,
            &progress,
        )
        .await?;
        download_asset(
            &client,
            &VAD,
            &staging.join("silero_vad.onnx"),
            &cancel,
            WHISPER.bytes,
            &progress,
        )
        .await?;
        let target = self.model_path();
        let extraction_cancel = cancel.clone();
        tokio::task::spawn_blocking(move || {
            let _cleanup = cleanup;
            extract_whisper(&staging.join("whisper.part"), &staging, &extraction_cancel)?;
            fs::remove_file(staging.join("whisper.part")).map_err(|e| e.to_string())?;
            let mut files = BTreeMap::new();
            for name in EXPECTED_FILES {
                files.insert(
                    name.to_owned(),
                    file_receipt(&staging.join(name), &extraction_cancel)?,
                );
            }
            let receipt = Receipt {
                whisper: WHISPER.sha256.into(),
                vad: VAD.sha256.into(),
                files,
            };
            fs::write(
                staging.join("receipt.json"),
                serde_json::to_vec(&receipt).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            check_canceled(&extraction_cancel)?;
            let backup = staging.with_extension("old");
            let had_old = target.exists();
            if had_old {
                fs::rename(&target, &backup).map_err(|e| e.to_string())?;
            }
            if let Err(error) = fs::rename(&staging, &target) {
                if had_old {
                    let _ = fs::rename(&backup, &target);
                }
                return Err(error.to_string());
            }
            if had_old {
                let _ = fs::remove_dir_all(backup);
            }
            Ok::<_, String>(())
        })
        .await
        .map_err(|e| e.to_string())??;
        self.status().await
    }
    pub async fn transcribe(
        &self,
        name: String,
        bytes: Vec<u8>,
        cancel: CancellationToken,
    ) -> Result<String, String> {
        if bytes.is_empty() || bytes.len() as u64 > MAX_AUDIO_BYTES {
            return Err("音频为空或超过 512 MiB。".into());
        }
        let permit = tokio::select! {
            _ = cancel.cancelled() => return Err("语音任务已取消。".into()),
            permit = self.inference.clone().acquire_owned() => permit.map_err(|e| e.to_string())?,
        };
        let root = self.model_path();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            check_canceled(&cancel)?;
            verified_install(&root, &cancel).map_err(|e| format!("本地语音模型未就绪：{e}"))?;
            let samples = decode_audio(&name, bytes, &cancel)?;
            recognize(&root, &samples, &cancel)
        })
        .await
        .map_err(|e| e.to_string())?
    }
}

async fn download_asset(
    client: &reqwest::Client,
    asset: &Asset,
    path: &Path,
    cancel: &CancellationToken,
    offset: u64,
    progress: &(impl Fn(DownloadProgress) + Send + Sync),
) -> Result<(), String> {
    let response = tokio::select! {
        _ = cancel.cancelled() => return Err("语音任务已取消。".into()),
        response = client.get(asset.url).send() => response.map_err(|e| e.to_string())?,
    }
    .error_for_status()
    .map_err(|e| format!("模型下载失败：{e}"))?;
    if response
        .content_length()
        .is_some_and(|length| length != asset.bytes)
    {
        return Err("模型下载大小不匹配。".into());
    }
    let mut stream = response.bytes_stream();
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await
        .map_err(|e| e.to_string())?;
    let mut downloaded = 0;
    let mut hash = Sha256::new();
    loop {
        let chunk = tokio::select! {
            _ = cancel.cancelled() => return Err("语音任务已取消。".into()),
            next = stream.next() => next,
        };
        let Some(chunk) = chunk else { break };
        let chunk = chunk.map_err(|e| e.to_string())?;
        downloaded += chunk.len() as u64;
        if downloaded > asset.bytes {
            return Err("模型下载大小超出清单。".into());
        }
        hash.update(&chunk);
        file.write_all(&chunk).await.map_err(|e| e.to_string())?;
        progress(DownloadProgress {
            downloaded: offset + downloaded,
            total: DOWNLOAD_BYTES,
        });
    }
    check_canceled(cancel)?;
    if downloaded != asset.bytes || format!("{:x}", hash.finalize()) != asset.sha256 {
        return Err("模型下载完整性校验失败。".into());
    }
    file.sync_all().await.map_err(|e| e.to_string())
}

fn extract_whisper(archive: &Path, root: &Path, cancel: &CancellationToken) -> Result<(), String> {
    let mut archive = tar::Archive::new(BzDecoder::new(
        File::open(archive).map_err(|e| e.to_string())?,
    ));
    let mut found = 0;
    for entry in archive.entries().map_err(|e| e.to_string())? {
        check_canceled(cancel)?;
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?.into_owned();
        if path
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
        {
            return Err("模型压缩包包含无效路径。".into());
        }
        if !(entry.header().entry_type().is_file() || entry.header().entry_type().is_dir()) {
            return Err("模型压缩包包含链接或特殊文件。".into());
        }
        let Some(name) = EXPECTED_FILES[..3]
            .iter()
            .find(|name| path == Path::new("sherpa-onnx-whisper-tiny").join(name))
        else {
            continue;
        };
        if !entry.header().entry_type().is_file()
            || entry.size() == 0
            || entry.size() > WHISPER.bytes * 4
        {
            return Err("模型压缩包文件大小无效。".into());
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(root.join(name))
            .map_err(|e| e.to_string())?;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            check_canceled(cancel)?;
            let n = entry.read(&mut buffer).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            file.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
        }
        file.sync_all().map_err(|e| e.to_string())?;
        found += 1;
    }
    if found != 3 {
        return Err("模型压缩包缺少必要文件。".into());
    }
    Ok(())
}

fn decode_audio(
    name: &str,
    bytes: Vec<u8>,
    cancel: &CancellationToken,
) -> Result<Vec<f32>, String> {
    let mut hint = Hint::new();
    if let Some(ext) = Path::new(name).extension().and_then(|v| v.to_str()) {
        hint.with_extension(ext);
    }
    let source = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            source,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("无法读取音频，请使用 WAV、MP3、M4A、OGG 或 FLAC：{e}"))?;
    let mut format = probed.format;
    let track = format.default_track().ok_or("音频文件没有音轨。")?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| e.to_string())?;
    let mut samples = Vec::new();
    let mut resampler = None;
    let mut input_rate = None;
    loop {
        check_canceled(cancel)?;
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(AudioError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(format!("音频解码失败：{e}")),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = decoder
            .decode(&packet)
            .map_err(|e| format!("音频解码失败：{e}"))?;
        let spec = *decoded.spec();
        if !(8_000..=192_000).contains(&spec.rate)
            || spec.channels.count() == 0
            || spec.channels.count() > 32
        {
            return Err("音频采样率或声道数无效。".into());
        }
        match input_rate {
            Some(rate) if rate != spec.rate => return Err("不支持中途改变采样率的音频。".into()),
            None => {
                input_rate = Some(spec.rate);
                if spec.rate != SAMPLE_RATE as u32 {
                    resampler = Some(
                        LinearResampler::create(spec.rate as i32, SAMPLE_RATE)
                            .ok_or("无法创建音频重采样器。")?,
                    );
                }
            }
            _ => {}
        }
        let mut converted = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        converted.copy_interleaved_ref(decoded);
        let mono: Vec<f32> = converted
            .samples()
            .chunks_exact(spec.channels.count())
            .map(|frame| frame.iter().sum::<f32>() / spec.channels.count() as f32)
            .collect();
        if mono.iter().any(|v| !v.is_finite()) {
            return Err("音频包含无效采样值。".into());
        }
        if let Some(resampler) = &resampler {
            samples.extend(resampler.resample(&mono, false));
        } else {
            samples.extend(mono);
        }
        if samples.len() > MAX_SAMPLES {
            return Err("单次音频不能超过两小时。".into());
        }
    }
    if let Some(resampler) = resampler {
        samples.extend(resampler.resample(&[], true));
    }
    if samples.is_empty() {
        return Err("音频文件没有可识别的采样。".into());
    }
    if samples.len() > MAX_SAMPLES {
        return Err("单次音频不能超过两小时。".into());
    }
    Ok(samples)
}

fn recognize(root: &Path, samples: &[f32], cancel: &CancellationToken) -> Result<String, String> {
    check_canceled(cancel)?;
    let path = |name: &str| root.join(name).to_string_lossy().into_owned();
    let config = OfflineRecognizerConfig {
        model_config: OfflineModelConfig {
            whisper: OfflineWhisperModelConfig {
                encoder: Some(path(EXPECTED_FILES[0])),
                decoder: Some(path(EXPECTED_FILES[1])),
                language: Some(String::new()),
                task: Some("transcribe".into()),
                tail_paddings: -1,
                ..Default::default()
            },
            tokens: Some(path(EXPECTED_FILES[2])),
            num_threads: 2,
            provider: Some("cpu".into()),
            ..Default::default()
        },
        decoding_method: Some("greedy_search".into()),
        ..Default::default()
    };
    let recognizer = OfflineRecognizer::create(&config).ok_or("无法启动 Whisper 本地识别引擎。")?;
    check_canceled(cancel)?;
    let vad_config = VadModelConfig {
        silero_vad: SileroVadModelConfig {
            model: Some(path(EXPECTED_FILES[3])),
            threshold: 0.5,
            min_speech_duration: 0.25,
            min_silence_duration: 0.65,
            window_size: VAD_WINDOW as i32,
            max_speech_duration: 20.0,
        },
        sample_rate: SAMPLE_RATE,
        num_threads: 1,
        provider: Some("cpu".into()),
        ..Default::default()
    };
    let vad =
        VoiceActivityDetector::create(&vad_config, 30.0).ok_or("无法启动 Silero 语音检测引擎。")?;
    let mut text = Vec::new();
    let mut drain = || -> Result<(), String> {
        while let Some(segment) = vad.front() {
            check_canceled(cancel)?;
            let stream = recognizer.create_stream();
            stream.accept_waveform(SAMPLE_RATE, segment.samples());
            recognizer.decode(&stream);
            check_canceled(cancel)?;
            let result = stream.get_result().ok_or("本地识别引擎没有返回结果。")?;
            let transcript = result.text.trim();
            if !transcript.is_empty() {
                text.push(transcript.to_owned());
            }
            vad.pop();
        }
        Ok(())
    };
    for chunk in samples.chunks(VAD_WINDOW) {
        check_canceled(cancel)?;
        if chunk.len() == VAD_WINDOW {
            vad.accept_waveform(chunk);
        } else {
            let mut last = [0f32; VAD_WINDOW];
            last[..chunk.len()].copy_from_slice(chunk);
            vad.accept_waveform(&last);
        }
        drain()?;
    }
    vad.flush();
    drain()?;
    check_canceled(cancel)?;
    if text.is_empty() {
        return Err("未检测到可识别的语音。".into());
    }
    Ok(text.join("\n"))
}

#[cfg(test)]
#[path = "../../test/native_stt.rs"]
mod tests;

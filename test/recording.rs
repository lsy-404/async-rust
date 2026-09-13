use super::*;

#[test]
fn callback_downmixes_and_converts_supported_formats() {
    let (tx, rx) = mpsc::sync_channel(4);
    let failure = AtomicU8::new(0);
    enqueue(&[1.0f32, -1.0, 0.5, 0.5], 2, &tx, &failure);
    assert_eq!(rx.recv().unwrap(), vec![0, 16384]);
    enqueue(&[i16::MIN, i16::MAX], 1, &tx, &failure);
    assert_eq!(rx.recv().unwrap(), vec![i16::MIN, i16::MAX]);
    enqueue(&[0u16, 32768, u16::MAX], 1, &tx, &failure);
    assert_eq!(rx.recv().unwrap(), vec![i16::MIN, 0, i16::MAX]);
    failure_message(&failure).unwrap();
}

#[test]
fn callback_is_bounded_and_reports_overflow_or_invalid_samples() {
    let (tx, rx) = mpsc::sync_channel(1);
    let failure = AtomicU8::new(0);
    enqueue(&vec![0f32; CHUNK_FRAMES * 3], 1, &tx, &failure);
    assert_eq!(rx.recv().unwrap().len(), CHUNK_FRAMES);
    assert!(failure_message(&failure).unwrap_err().contains("溢出"));
    failure.store(0, Ordering::Relaxed);
    enqueue(&[f32::NAN], 1, &tx, &failure);
    assert!(failure_message(&failure).unwrap_err().contains("无效"));
    failure.store(0, Ordering::Relaxed);
    enqueue(&[0i16], 2, &tx, &failure);
    assert!(failure_message(&failure).is_err());
}

#[test]
fn transcript_handoff_aggregates_small_callbacks_and_flushes_stop_tail() {
    let (sender, receiver) = mpsc::sync_channel(4);
    let mut pending = Vec::new();
    let input: Vec<i16> = (0..(CHUNK_FRAMES * 2 + 317))
        .map(|value| value as i16)
        .collect();
    for part in input.chunks(512) {
        forward_transcript_audio(&mut pending, part, &sender, false).unwrap();
    }
    forward_transcript_audio(&mut pending, &[], &sender, true).unwrap();
    let chunks: Vec<Vec<i16>> = receiver.try_iter().collect();
    assert_eq!(
        chunks.iter().map(Vec::len).collect::<Vec<_>>(),
        vec![CHUNK_FRAMES, CHUNK_FRAMES, 317]
    );
    assert_eq!(chunks.concat(), input);
}

#[test]
fn wav_writer_persists_source_rate_and_cleans_canceled_or_oversized_file() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("capture.wav");
    let mut capture = WavCapture::create(path.clone(), 48_000).unwrap();
    capture.write(&[100, -200, 300]).unwrap();
    assert_eq!(capture.finish().unwrap(), path);
    let mut reader = hound::WavReader::open(&path).unwrap();
    assert_eq!(reader.spec().sample_rate, 48_000);
    assert_eq!(reader.spec().channels, 1);
    assert_eq!(reader.spec().bits_per_sample, 16);
    assert_eq!(
        reader
            .samples::<i16>()
            .map(Result::unwrap)
            .collect::<Vec<_>>(),
        vec![100, -200, 300]
    );
    assert!(!path.with_extension("wav.part").exists());
    let path = temp.path().join("canceled.wav");
    {
        let mut capture = WavCapture::create(path.clone(), 44_100).unwrap();
        capture.max_samples = 2;
        capture.write(&[1, 2]).unwrap();
        assert!(capture.write(&[3]).unwrap_err().contains("上限"));
    }
    assert!(!path.exists());
    assert!(!path.with_extension("wav.part").exists());
}

fn fake_active(session_id: &str, canceled: Arc<AtomicU8>) -> ActiveRecording {
    let (control, commands) = mpsc::channel();
    let worker = thread::spawn(move || match commands.recv().unwrap() {
        Control::Cancel => {
            canceled.store(1, Ordering::Release);
            Ok(None)
        }
        Control::Stop => Ok(Some(PathBuf::from("stopped.wav"))),
    });
    ActiveRecording {
        session_id: session_id.into(),
        control,
        canceled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        callback_gate: Arc::new(StdMutex::new(())),
        worker: Some(worker),
    }
}

#[test]
fn cancellation_gate_rejects_late_transcript_persistence() {
    let canceled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let gate = Arc::new(StdMutex::new(()));
    let persisted = Arc::new(StdMutex::new(Vec::new()));
    let destination = persisted.clone();
    let callback = guarded_callback(
        canceled.clone(),
        gate.clone(),
        Arc::new(move |text| {
            destination.lock().unwrap().push(text);
            Ok(())
        }),
    );
    callback("before".into()).unwrap();
    {
        let _guard = gate.lock().unwrap();
        canceled.store(true, Ordering::Release);
    }
    callback("after".into()).unwrap();
    assert_eq!(&*persisted.lock().unwrap(), &["before"]);
}

#[tokio::test]
async fn lifecycle_pins_session_and_cancel_or_drop_joins_worker() {
    let canceled = Arc::new(AtomicU8::new(0));
    let manager = RecordingManager::new();
    *manager.active.lock().await = Some(fake_active("original", canceled.clone()));
    assert!(manager
        .stop("other")
        .await
        .unwrap_err()
        .contains("其他会话"));
    assert!(manager
        .start(
            "new",
            Path::new("unused"),
            &SttManager::new(PathBuf::from("unused")),
            RecordingSource::Microphone,
            None,
            Arc::new(|_| Ok(())),
            Arc::new(|_| {}),
            Arc::new(|_| {}),
        )
        .await
        .unwrap_err()
        .contains("已有"));
    assert!(manager.active.lock().await.is_some());
    manager.cancel().await.unwrap();
    assert_eq!(canceled.load(Ordering::Acquire), 1);
    assert!(manager.active.lock().await.is_none());
    canceled.store(0, Ordering::Release);
    *manager.active.lock().await = Some(fake_active("original", canceled.clone()));
    drop(manager);
    assert_eq!(canceled.load(Ordering::Acquire), 1);
}

#[test]
fn level_meter_rate_limits_and_normalizes() {
    let mut meter = LevelMeter::new();
    let loud = vec![i16::MAX; 256];
    let first = meter.sample(&loud);
    assert!(first.is_some());
    assert!((first.unwrap() - 1.0).abs() < 1e-4);
    // A second sample immediately after must be suppressed by the rate limit.
    assert!(meter.sample(&loud).is_none());
    assert!(meter.sample(&[]).is_none());
    let quiet = vec![0i16; 256];
    let mut fresh = LevelMeter::new();
    assert_eq!(fresh.sample(&quiet), Some(0.0));
}

#[test]
fn recording_event_serializes_with_a_type_tag_and_camel_case_fields() {
    let transcript = crate::RecordingEvent::Transcript {
        session_id: "s".into(),
        text: "hello".into(),
    };
    assert_eq!(
        serde_json::to_value(&transcript).unwrap(),
        serde_json::json!({"type":"transcript","sessionId":"s","text":"hello"})
    );
    let level = crate::RecordingEvent::Level {
        session_id: "s".into(),
        level: 0.5,
    };
    assert_eq!(
        serde_json::to_value(&level).unwrap(),
        serde_json::json!({"type":"level","sessionId":"s","level":0.5})
    );
}

#[test]
fn abandoned_stop_result_removes_unclaimed_recording() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("unclaimed.wav");
    fs::write(&path, b"audio").unwrap();
    drop(CompletedCapture(Some(path.clone())));
    assert!(!path.exists());
    fs::write(&path, b"audio").unwrap();
    assert_eq!(CompletedCapture(Some(path.clone())).claim().unwrap(), path);
    assert!(path.exists());
}

#[test]
fn recording_source_serializes_as_camel_case_to_match_the_frontend_capture_mode() {
    assert_eq!(
        serde_json::to_value(RecordingSource::Microphone).unwrap(),
        serde_json::json!("microphone")
    );
    assert_eq!(
        serde_json::to_value(RecordingSource::SystemAudio).unwrap(),
        serde_json::json!("systemAudio")
    );
    assert_eq!(
        serde_json::from_value::<RecordingSource>(serde_json::json!("microphone")).unwrap(),
        RecordingSource::Microphone
    );
    assert_eq!(
        serde_json::from_value::<RecordingSource>(serde_json::json!("systemAudio")).unwrap(),
        RecordingSource::SystemAudio
    );
}

#[test]
fn macos_version_parsing_handles_two_and_three_component_strings_and_garbage() {
    assert_eq!(parse_macos_version("14.6.1"), Some((14, 6)));
    assert_eq!(parse_macos_version("14.6"), Some((14, 6)));
    assert_eq!(parse_macos_version("15"), Some((15, 0)));
    assert_eq!(parse_macos_version("  13.2  \n"), Some((13, 2)));
    assert_eq!(parse_macos_version(""), None);
    assert_eq!(parse_macos_version("not-a-version"), None);
}

#[test]
fn macos_system_audio_gate_requires_at_least_the_min_supported_release() {
    assert!(!macos_supports_system_audio(None));
    assert!(!macos_supports_system_audio(Some((14, 5))));
    assert!(!macos_supports_system_audio(Some((13, 9))));
    assert!(macos_supports_system_audio(Some((14, 6))));
    assert!(macos_supports_system_audio(Some((14, 7))));
    assert!(macos_supports_system_audio(Some((15, 0))));
}

#[test]
fn gate_for_passes_available_capabilities_and_explains_unavailable_ones() {
    gate_for(&SystemAudioCapability {
        available: true,
        reason: None,
    })
    .unwrap();
    let os_error = gate_for(&SystemAudioCapability {
        available: false,
        reason: Some("unsupported-os".into()),
    })
    .unwrap_err();
    assert!(os_error.contains("14.6"));
    let platform_error = gate_for(&SystemAudioCapability {
        available: false,
        reason: Some("unsupported-platform".into()),
    })
    .unwrap_err();
    assert!(platform_error.contains("平台"));
}

// Fraction of non-whitespace, non-punctuation characters that are CJK Unified
// Ideographs; used to tell real Chinese text apart from romanisation/English.
fn cjk_character_ratio(text: &str) -> f32 {
    let mut total = 0usize;
    let mut cjk = 0usize;
    for ch in text.chars() {
        if ch.is_whitespace() || ch.is_ascii_punctuation() || "，。！？、；：".contains(ch) {
            continue;
        }
        total += 1;
        if ('\u{4E00}'..='\u{9FFF}').contains(&ch) {
            cjk += 1;
        }
    }
    if total == 0 {
        0.0
    } else {
        cjk as f32 / total as f32
    }
}

fn read_i16_wav(path: &Path) -> (u32, Vec<i16>) {
    let mut reader = hound::WavReader::open(path).unwrap();
    let rate = reader.spec().sample_rate;
    let samples = reader.samples::<i16>().map(Result::unwrap).collect();
    (rate, samples)
}

// Exercises the exact bounded-queue -> VAD -> incremental-transcription
// pipeline `start_recording` wires up (see `recording_worker` /
// `transcription_worker`), but feeds it real `say`-generated speech resampled
// to a realistic capture rate instead of a live microphone, so it can run
// against an installed local model without hardware.
#[test]
#[ignore = "Set ASYNC_STT_MODEL_ROOT and ASYNC_STT_LONG_WAV to run the live incremental pipeline against real `say`-generated speech on an installed local model"]
fn actual_live_pipeline_streams_growing_snapshots_and_flushes_tail_for_long_clip() {
    let model_root = PathBuf::from(std::env::var("ASYNC_STT_MODEL_ROOT").unwrap());
    let source = PathBuf::from(std::env::var("ASYNC_STT_LONG_WAV").unwrap());
    let (native_rate, native_samples) = read_i16_wav(&source);
    // Resample up to a realistic microphone capture rate so this also exercises
    // the same streaming resampler path RecordingManager feeds LiveTranscriber through.
    let capture_rate = 48_000u32;
    let as_f32: Vec<f32> = native_samples
        .iter()
        .map(|sample| *sample as f32 / 32768.0)
        .collect();
    let resampler =
        sherpa_onnx::LinearResampler::create(native_rate as i32, capture_rate as i32).unwrap();
    let captured_f32 = resampler.resample(&as_f32, true);
    let captured: Vec<i16> = captured_f32
        .iter()
        .map(|sample| (sample.clamp(-1.0, 1.0) * 32767.0) as i16)
        .collect();

    let mut live = SttManager::new(model_root)
        .live_transcriber(capture_rate, Some("en".into()))
        .unwrap();
    let mut snapshots: Vec<String> = Vec::new();
    let mut cumulative = String::new();
    let mut first_segment_at_chunk = None;
    let chunk_duration = Duration::from_secs_f32(CHUNK_FRAMES as f32 / capture_rate as f32);
    let started = std::time::Instant::now();
    let total_chunks = captured.len().div_ceil(CHUNK_FRAMES);
    for (index, chunk) in captured.chunks(CHUNK_FRAMES).enumerate() {
        for segment in live.accept_i16(chunk).unwrap() {
            if !cumulative.is_empty() {
                cumulative.push(' ');
            }
            cumulative.push_str(&segment);
            snapshots.push(cumulative.clone());
            first_segment_at_chunk.get_or_insert(index);
        }
        // Match the CPAL handoff cadence a real microphone stream would produce.
        thread::sleep(chunk_duration);
    }
    let before_finish = snapshots.len();
    assert!(
        before_finish > 0,
        "no transcript snapshot arrived before the stream ended"
    );
    let first = first_segment_at_chunk.unwrap();
    assert!(
        first < total_chunks * 9 / 10,
        "first snapshot arrived too late to prove real incremental streaming: chunk {first} of {total_chunks}"
    );
    for segment in live.finish().unwrap() {
        if !cumulative.is_empty() {
            cumulative.push(' ');
        }
        cumulative.push_str(&segment);
        snapshots.push(cumulative.clone());
    }
    let elapsed = started.elapsed();
    eprintln!(
        "live long-clip {capture_rate} Hz: snapshots_before_finish={before_finish}, \
         total_snapshots={}, elapsed={:.2}s, final=\"{cumulative}\"",
        snapshots.len(),
        elapsed.as_secs_f32(),
    );
    assert!(
        snapshots.len() >= 3,
        "expected multiple VAD segments across the ~50s clip, got {}",
        snapshots.len()
    );
    // Monotonically growing: each cumulative snapshot must never shrink or
    // lose content the previous snapshot already reported.
    for pair in snapshots.windows(2) {
        assert!(
            pair[1].len() >= pair[0].len() && pair[1].starts_with(pair[0].as_str()),
            "snapshot regressed: {:?} -> {:?}",
            pair[0],
            pair[1]
        );
    }
    let final_text = cumulative.to_lowercase();
    assert!(
        final_text.contains("brown fox") || final_text.contains("lazy dog"),
        "missing early-sentence content: {final_text}"
    );
    // The closing sentence only ever completes via `finish()`'s VAD flush; the
    // capture loop stops mid-utterance, so it can only appear in the tail flush.
    assert!(
        final_text.contains("thank you") || final_text.contains("listening"),
        "final snapshot is missing the flushed tail segment: {final_text}"
    );
}

#[test]
#[ignore = "Set ASYNC_STT_MODEL_ROOT and ASYNC_STT_ZH_WAV to run the live incremental pipeline against real `say`-generated Mandarin speech on an installed local model"]
fn actual_live_pipeline_recognizes_mandarin_and_flushes_tail() {
    let model_root = PathBuf::from(std::env::var("ASYNC_STT_MODEL_ROOT").unwrap());
    let source = PathBuf::from(std::env::var("ASYNC_STT_ZH_WAV").unwrap());
    let (rate, samples) = read_i16_wav(&source);
    let mut live = SttManager::new(model_root)
        .live_transcriber(rate, Some("zh".into()))
        .unwrap();
    let mut cumulative = String::new();
    let mut segment_count = 0usize;
    let chunk_duration = Duration::from_secs_f32(CHUNK_FRAMES as f32 / rate as f32);
    for chunk in samples.chunks(CHUNK_FRAMES) {
        for segment in live.accept_i16(chunk).unwrap() {
            cumulative.push_str(&segment);
            segment_count += 1;
        }
        thread::sleep(chunk_duration);
    }
    for segment in live.finish().unwrap() {
        cumulative.push_str(&segment);
        segment_count += 1;
    }
    eprintln!("live mandarin transcript ({segment_count} segments): {cumulative}");
    assert!(segment_count > 0, "no mandarin segments were produced");
    let ratio = cjk_character_ratio(&cumulative);
    assert!(
        ratio > 0.5,
        "live mandarin output is not mostly Chinese characters (ratio={ratio}): {cumulative}"
    );
    assert!(
        cumulative.contains("天气") || cumulative.contains("谢谢") || cumulative.contains("测试"),
        "live mandarin output is missing expected content: {cumulative}"
    );
    // The live and one-shot paths must never disagree on script: assert Simplified
    // explicitly, and fail loudly if a Traditional form leaks through instead
    // (this is exactly the bug the stt.rs normalisation point exists to prevent).
    assert!(
        !cumulative.contains('氣') && !cumulative.contains('謝') && !cumulative.contains('認'),
        "live mandarin output contains Traditional Chinese characters, script normalisation regressed: {cumulative}"
    );
}

// Mirrors the real cancel path: `ActiveRecording::finish(Control::Cancel)`
// takes `callback_gate` and flips `canceled` before the writer thread ever
// stops feeding the transcript queue, then the queue is closed with no final
// flush. This drives `transcription_worker` (the same function
// `recording_worker` spawns) and the same `guarded_callback` wrapper, so a
// real cancellation race runs against the real model.
#[test]
#[ignore = "Set ASYNC_STT_MODEL_ROOT and ASYNC_STT_LONG_WAV to run the live pipeline's cancellation path against an installed local model"]
fn actual_live_pipeline_cancel_mid_stream_stops_cleanly_and_persists_nothing() {
    let model_root = PathBuf::from(std::env::var("ASYNC_STT_MODEL_ROOT").unwrap());
    let source = PathBuf::from(std::env::var("ASYNC_STT_LONG_WAV").unwrap());
    let (rate, samples) = read_i16_wav(&source);
    let mut live = SttManager::new(model_root)
        .live_transcriber(rate, Some("en".into()))
        .unwrap();
    let (sender, receiver) = mpsc::sync_channel::<Vec<i16>>(QUEUE_CHUNKS);
    let persisted = Arc::new(StdMutex::new(Vec::<String>::new()));
    let destination = persisted.clone();
    let canceled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let gate = Arc::new(StdMutex::new(()));
    let callback = guarded_callback(
        canceled.clone(),
        gate.clone(),
        Arc::new(move |text| {
            destination.lock().unwrap().push(text);
            Ok(())
        }),
    );
    let worker_canceled = canceled.clone();
    let worker = thread::spawn(move || {
        transcription_worker(&mut live, receiver, callback, worker_canceled)
    });

    let chunk_duration = Duration::from_secs_f32(CHUNK_FRAMES as f32 / rate as f32);
    // Cut off once a real segment has actually been persisted, rather than at a
    // fixed chunk count: a fixed cutoff is flaky, since it races the model's own
    // (machine-dependent) decode speed instead of the condition the test cares
    // about.
    for chunk in samples.chunks(CHUNK_FRAMES) {
        if !persisted.lock().unwrap().is_empty() {
            break;
        }
        if sender.send(chunk.to_vec()).is_err() {
            break;
        }
        thread::sleep(chunk_duration);
    }
    // Same sequencing as `ActiveRecording::finish(Control::Cancel)`: gate,
    // flip the flag, THEN stop feeding audio (no final flush chunk).
    {
        let _guard = gate.lock().unwrap();
        canceled.store(true, Ordering::Release);
    }
    drop(sender);
    worker
        .join()
        .expect("transcription worker panicked")
        .expect("transcription worker returned an error instead of stopping cleanly");

    let received = persisted.lock().unwrap().clone();
    eprintln!("segments persisted before cancellation: {received:?}");
    assert!(
        !received.is_empty(),
        "expected at least one real segment to have been persisted before cancellation, but the \
         feed loop ran out of audio without the worker ever persisting one"
    );
    let joined = received.join(" ").to_lowercase();
    assert!(
        !joined.contains("thank you") && !joined.contains("final sentence"),
        "content from well past the cancellation point leaked into what was persisted: {joined}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn system_audio_capability_reason_is_present_exactly_when_unavailable() {
    let capability = system_audio_capability();
    assert_eq!(capability.available, capability.reason.is_none());
}

#[cfg(target_os = "windows")]
#[test]
fn system_audio_capability_is_always_available_on_windows() {
    let capability = system_audio_capability();
    assert!(capability.available);
    assert!(capability.reason.is_none());
}

// Building a real loopback stream needs an actual output device and, on
// macOS, an interactively-granted System Audio Recording permission; neither
// is available in CI. Mirrors the ASYNC_STT_* convention in test/native_stt.rs.
#[tokio::test]
#[ignore = "Set ASYNC_RECORDING_SYSTEM_AUDIO=1 on a machine with a real output device (and, on macOS 14.6+, granted System Audio Recording permission) to run"]
async fn system_audio_capture_starts_and_stops_against_real_hardware() {
    std::env::var("ASYNC_RECORDING_SYSTEM_AUDIO").unwrap();
    let temp = tempfile::tempdir().unwrap();
    let manager = RecordingManager::new();
    manager
        .start(
            "hardware-check",
            temp.path(),
            &SttManager::new(temp.path().join("stt")),
            RecordingSource::SystemAudio,
            None,
            Arc::new(|_| Ok(())),
            Arc::new(|_| {}),
            Arc::new(|_| {}),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    manager.stop("hardware-check").await.unwrap();
}

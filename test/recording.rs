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
            Arc::new(|_| Ok(())),
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

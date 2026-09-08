use super::*;
use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

fn wave(rate: u32, channels: u16, samples: impl IntoIterator<Item = i16>) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(
            &mut bytes,
            hound::WavSpec {
                channels,
                sample_rate: rate,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for sample in samples {
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();
    }
    bytes.into_inner()
}

#[test]
fn decodes_and_resamples_real_capture_rates_and_stereo() {
    for rate in [16_000, 44_100, 48_000] {
        let bytes = wave(
            rate,
            2,
            (0..rate).flat_map(|n| {
                let amplitude = ((n as f32 * 440.0 * 2.0 * std::f32::consts::PI / rate as f32)
                    .sin()
                    * 16_000.0) as i16;
                [amplitude, amplitude]
            }),
        );
        let output = decode_audio("recording.wav", bytes, &CancellationToken::new()).unwrap();
        assert!(
            (output.len() as i32 - SAMPLE_RATE).abs() <= 1,
            "rate={rate}, length={}",
            output.len()
        );
        assert!(output.iter().all(|v| v.is_finite()));
        assert!(output.iter().any(|v| v.abs() > 0.4));
    }
}

#[test]
fn rejects_invalid_audio_and_observes_cancellation() {
    assert!(decode_audio("broken.wav", vec![0; 64], &CancellationToken::new()).is_err());
    let canceled = CancellationToken::new();
    canceled.cancel();
    let bytes = wave(16_000, 1, [0; 1024]);
    assert!(decode_audio("recording.wav", bytes, &canceled)
        .unwrap_err()
        .contains("取消"));
}

#[tokio::test]
async fn status_requires_complete_files_and_valid_digest() {
    let temp = tempfile::tempdir().unwrap();
    let manager = SttManager::new(temp.path().to_owned());
    assert!(!manager.status().await.unwrap().ready);
    fs::create_dir(manager.model_path()).unwrap();
    let mut files = BTreeMap::new();
    for name in EXPECTED_FILES {
        let path = manager.model_path().join(name);
        fs::write(&path, b"test model integrity").unwrap();
        files.insert(
            name.into(),
            file_receipt(&path, &CancellationToken::new()).unwrap(),
        );
    }
    let receipt = Receipt {
        whisper: WHISPER.sha256.into(),
        vad: VAD.sha256.into(),
        files,
    };
    fs::write(
        manager.model_path().join("receipt.json"),
        serde_json::to_vec(&receipt).unwrap(),
    )
    .unwrap();
    assert!(manager.status().await.unwrap().ready);
    fs::write(
        manager.model_path().join(EXPECTED_FILES[0]),
        b"corrupt model bytes!",
    )
    .unwrap();
    assert!(!manager.status().await.unwrap().ready);
    fs::remove_file(manager.model_path().join(EXPECTED_FILES[1])).unwrap();
    assert!(!manager.status().await.unwrap().ready);
}

#[tokio::test]
async fn model_download_enforces_lengths_hash_and_cancel_cleanup() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"model".to_vec()))
        .mount(&server)
        .await;
    let url: &'static str = Box::leak(server.uri().into_boxed_str());
    let hash: &'static str = Box::leak(format!("{:x}", Sha256::digest(b"model")).into_boxed_str());
    let asset = Asset {
        url,
        bytes: 5,
        sha256: hash,
    };
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("ok.part");
    download_asset(
        &reqwest::Client::new(),
        &asset,
        &path,
        &CancellationToken::new(),
        0,
        &|_| {},
    )
    .await
    .unwrap();
    assert_eq!(fs::read(path).unwrap(), b"model");
    let invalid = Asset {
        url,
        bytes: 6,
        sha256: hash,
    };
    assert!(download_asset(
        &reqwest::Client::new(),
        &invalid,
        &temp.path().join("wrong-length"),
        &CancellationToken::new(),
        0,
        &|_| {}
    )
    .await
    .is_err());
    let invalid = Asset {
        url,
        bytes: 5,
        sha256: "invalid",
    };
    assert!(download_asset(
        &reqwest::Client::new(),
        &invalid,
        &temp.path().join("wrong-digest"),
        &CancellationToken::new(),
        0,
        &|_| {}
    )
    .await
    .is_err());
    let staging = temp.path().join("cancelled");
    fs::create_dir(&staging).unwrap();
    {
        let _guard = StagingGuard(staging.clone());
        let cancel = CancellationToken::new();
        let cancel_progress = cancel.clone();
        assert!(download_asset(
            &reqwest::Client::new(),
            &asset,
            &staging.join("download.part"),
            &cancel,
            0,
            &move |_| cancel_progress.cancel()
        )
        .await
        .unwrap_err()
        .contains("取消"));
    }
    assert!(!staging.exists());
}

#[test]
fn archive_only_extracts_expected_files_and_rejects_links() {
    for linked in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let archive_path = temp.path().join("model.tar.bz2");
        let encoder = bzip2::write::BzEncoder::new(
            File::create(&archive_path).unwrap(),
            bzip2::Compression::default(),
        );
        let mut builder = tar::Builder::new(encoder);
        for name in EXPECTED_FILES[..3]
            .iter()
            .chain(std::iter::once(&"unrelated.onnx"))
        {
            let mut header = tar::Header::new_gnu();
            header.set_size(4);
            header.set_mode(0o600);
            header.set_cksum();
            builder
                .append_data(
                    &mut header,
                    format!("sherpa-onnx-whisper-tiny/{name}"),
                    &b"data"[..],
                )
                .unwrap();
        }
        if linked {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o600);
            builder
                .append_link(&mut header, "bad-link", "/tmp/unwanted")
                .unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
        let output = temp.path().join("extracted");
        fs::create_dir(&output).unwrap();
        let result = extract_whisper(&archive_path, &output, &CancellationToken::new());
        if linked {
            assert!(result.unwrap_err().contains("链接"));
        } else {
            result.unwrap();
            assert!(EXPECTED_FILES[..3]
                .iter()
                .all(|name| output.join(name).is_file()));
            assert!(!output.join("unrelated.onnx").exists());
        }
    }
}

#[tokio::test]
async fn missing_models_do_not_need_api_credentials_or_leave_busy_session() {
    let temp = tempfile::tempdir().unwrap();
    let state = crate::AppState::open(temp.path().join("state.sqlite3")).unwrap();
    let db = state.db().unwrap();
    db.execute("INSERT INTO workspaces VALUES('w','Class')", [])
        .unwrap();
    db.execute(
        "INSERT INTO sessions(id,workspace_id,title) VALUES('s','w','Lesson')",
        [],
    )
    .unwrap();
    for _ in 0..2 {
        let failure =
            crate::transcribe(&state, "s", "voice.wav".into(), wave(16_000, 1, [0; 1024]))
                .await
                .unwrap_err();
        assert!(failure.contains("模型未就绪"), "{failure}");
        assert!(state.cancellations.lock().unwrap().is_empty());
    }
}

#[tokio::test]
#[ignore = "Downloads the real IRIS model pack and a public JFK audio fixture into test/runtime"]
async fn actual_whisper_jfk_recognizes_capture_rates_and_persists_without_key() {
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("test/runtime/native-stt");
    fs::create_dir_all(&runtime).unwrap();
    let state = crate::AppState::open(runtime.join("fixture.sqlite3")).unwrap();
    state.stt.install(|_| {}).await.unwrap();
    assert!(state.stt.status().await.unwrap().ready);
    let fixture = runtime.join("jfk.wav");
    if !fixture.exists() {
        let bytes = reqwest::get(
            "https://raw.githubusercontent.com/ggerganov/whisper.cpp/master/samples/jfk.wav",
        )
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .bytes()
        .await
        .unwrap();
        fs::write(&fixture, bytes).unwrap();
    }
    let db = state.db().unwrap();
    db.execute(
        "INSERT OR IGNORE INTO workspaces VALUES('fixture','Speech test')",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT OR REPLACE INTO sessions(id,workspace_id,title) VALUES('jfk','fixture','Speech')",
        [],
    )
    .unwrap();
    let original = fs::read(&fixture).unwrap();
    let samples = decode_audio("jfk.wav", original.clone(), &CancellationToken::new()).unwrap();
    for rate in [16_000, 44_100, 48_000] {
        let bytes = if rate == 16_000 {
            original.clone()
        } else {
            let resampler = LinearResampler::create(16_000, rate).unwrap();
            wave(
                rate as u32,
                1,
                resampler
                    .resample(&samples, true)
                    .into_iter()
                    .map(|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16),
            )
        };
        let output = crate::transcribe(&state, "jfk", "capture.wav".into(), bytes)
            .await
            .unwrap();
        eprintln!("{rate} Hz transcript: {output}");
        let normalized = output
            .rsplit("\n\n")
            .next()
            .unwrap()
            .to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(normalized.contains("your country can do for you"));
        assert!(normalized.contains("what you can do for your country"));
        let stored: String = db
            .query_row(
                "SELECT transcription FROM sessions WHERE id='jfk'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, output);
        assert_eq!(
            output.matches("country").count(),
            if rate == 16_000 {
                2
            } else if rate == 44_100 {
                4
            } else {
                6
            }
        );
    }
    let before_cancel: String = db
        .query_row(
            "SELECT transcription FROM sessions WHERE id='jfk'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let (result, ()) = tokio::join!(
        crate::transcribe(&state, "jfk", "capture.wav".into(), original.clone()),
        async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            state
                .cancellations
                .lock()
                .unwrap()
                .get("jfk")
                .unwrap()
                .cancel();
        }
    );
    assert!(result.unwrap_err().contains("取消"));
    assert!(state.cancellations.lock().unwrap().is_empty());
    let after_cancel: String = db
        .query_row(
            "SELECT transcription FROM sessions WHERE id='jfk'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(before_cancel, after_cancel);
    let canceled = CancellationToken::new();
    canceled.cancel();
    assert!(state
        .stt
        .transcribe("jfk.wav".into(), original, canceled)
        .await
        .unwrap_err()
        .contains("取消"));
    let silence = wave(16_000, 1, std::iter::repeat_n(0, 16_000 * 2));
    assert!(state
        .stt
        .transcribe("silence.wav".into(), silence, CancellationToken::new())
        .await
        .unwrap_err()
        .contains("未检测到"));
}

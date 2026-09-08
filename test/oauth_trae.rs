use super::*;
use p256::ecdsa::signature::Verifier;
use wiremock::{matchers::path, Mock, MockServer, ResponseTemplate};

fn test_credential() -> TraeCredential {
    TraeCredential {
        access: "test-access".into(),
        refresh: "test-refresh".into(),
        expires: now() + 3_600_000,
        refresh_expires: now() + 86_400_000,
        host: HOSTS[0].into(),
        client_id: CLIENT_ID.into(),
        device: device().unwrap(),
        account_id: "test-account".into(),
        label: "Account".into(),
        store_country: "CA".into(),
        models: vec![],
    }
}
#[test]
fn callback_requires_exact_correlation_host_and_scope() {
    let state = CallbackState {
        authority: "127.0.0.1:8765".into(),
        trace: "correlation".into(),
        sender: Mutex::new(None),
    };
    let mut url = Url::parse("http://127.0.0.1:8765/authorize").unwrap();
    url.query_pairs_mut().extend_pairs(&[
        ("loginTraceID", "correlation"),
        ("authCodeInfo", "{\"AuthCode\":\"test-code\"}"),
        ("userTag", "row"),
        ("scope", "trae"),
    ]);
    let target = format!("{}?{}", url.path(), url.query().unwrap());
    assert_eq!(
        callback_value("GET", &state.authority, &target, &state).unwrap(),
        Some(("test-code".into(), "row".into()))
    );
    for (method, host, target) in [
        ("POST", state.authority.as_str(), target.clone()),
        ("GET", "localhost:8765", target.clone()),
        (
            "GET",
            state.authority.as_str(),
            format!("{target}&loginTraceID=evil"),
        ),
        (
            "GET",
            state.authority.as_str(),
            target.replace("scope=trae", "scope=other"),
        ),
    ] {
        assert!(callback_value(method, host, &target, &state).is_err());
    }
}
#[test]
fn refresh_proof_is_bound_to_device_and_token() {
    let credential = test_credential();
    let body = refresh_body(&credential, 12345, "nonce").unwrap();
    let signature = STANDARD
        .decode(body["DeviceProof"]["Signature"].as_str().unwrap())
        .unwrap();
    let signature = Signature::from_der(&signature).unwrap();
    let key = validate(&credential).unwrap();
    let proof = format!("POST\n{EXCHANGE}\n{CLIENT_ID}\ntest-refresh\n12345\nnonce");
    key.verifying_key()
        .verify(proof.as_bytes(), &signature)
        .unwrap();
    assert!(key
        .verifying_key()
        .verify(b"different-token", &signature)
        .is_err());
}
#[test]
fn encrypted_messages_roundtrip_with_protocol_key_and_aad() {
    let pin = [1; 8];
    let iv = [2; 12];
    let timestamp = "12345";
    let encrypted = encrypted_messages(
        &[json!({"role":"user","content":"你好"})],
        &pin,
        &iv,
        timestamp,
    )
    .unwrap();
    let bytes = STANDARD.decode(encrypted).unwrap();
    assert_eq!(&bytes[..12], &iv);
    let mut key = [
        0x61, 0x95, 0xf2, 0x4c, 0xa4, 0xd4, 0x30, 0xf8, 0xa4, 0x83, 0x3d, 0xe7, 0xdb, 0x8d, 0xac,
        0x37, 0xd1, 0x48, 0xa0, 0x84, 0xe7, 0x46, 0x4a, 0x35, 0x1f, 0xfa, 0x68, 0x58, 0x5c, 0x16,
        0xb9, 0x55,
    ];
    for i in 0..8 {
        key[i] ^= pin[i];
    }
    let cipher = Aes256Gcm::new_from_slice(&key).unwrap();
    let plain = cipher
        .decrypt(
            (&iv).into(),
            Payload {
                msg: &bytes[12..],
                aad: timestamp.as_bytes(),
            },
        )
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&plain).unwrap(),
        json!([{"role":"user","content":[{"type":"text","text":"你好"}]}])
    );
    assert!(cipher
        .decrypt(
            (&iv).into(),
            Payload {
                msg: &bytes[12..],
                aad: b"altered"
            }
        )
        .is_err());
}
#[test]
fn credentials_validate_host_binding_country_and_expiry() {
    let mut credential = test_credential();
    assert!(validate(&credential).is_ok());
    assert_eq!(
        inference_host(&credential).unwrap(),
        "https://coresg-normal.trae.ai"
    );
    credential.store_country = "US".into();
    assert_eq!(
        inference_host(&credential).unwrap(),
        "https://core-normal.traeapi.us"
    );
    credential.store_country.clear();
    assert!(inference_host(&credential).is_err());
    credential.host = "https://untrusted.example".into();
    assert!(validate(&credential).is_err());
    assert!(expiry(&json!(0), None).is_err());
    assert!(update_tokens(&mut credential, &json!({"Token":"test"})).is_err());
}
#[tokio::test]
async fn streams_only_text_and_rejects_incomplete_or_malformed_events() {
    let server = MockServer::start().await;
    let body = "event: message\ndata: {\"reasoning_content\":\"hidden\"}\n\nevent: message\ndata: {\"response\":\"visible\"}\n\nevent: done\ndata: {\"finish_reason\":\"stop\"}\n\n";
    Mock::given(path("/valid"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(&server)
        .await;
    let deltas = Mutex::new(Vec::new());
    let result = consume_chat(
        reqwest::get(format!("{}/valid", server.uri()))
            .await
            .unwrap(),
        |delta| {
            deltas.lock().unwrap().push(delta);
            Ok(())
        },
    )
    .await
    .unwrap();
    assert_eq!(result, "visible");
    assert_eq!(*deltas.lock().unwrap(), vec!["visible"]);
    for (index, body) in [
        "event: message\ndata: {\"response\":\"partial\"}\n\n",
        "event: message\ndata: invalid\n\n",
        "event: error\ndata: {\"secret\":\"never return this\"}\n\n",
    ]
    .iter()
    .enumerate()
    {
        let path_value = format!("/{index}");
        Mock::given(path(path_value.clone()))
            .respond_with(ResponseTemplate::new(200).set_body_raw(*body, "text/event-stream"))
            .mount(&server)
            .await;
        let error = consume_chat(
            reqwest::get(format!("{}{path_value}", server.uri()))
                .await
                .unwrap(),
            |_| Ok(()),
        )
        .await
        .unwrap_err();
        assert!(!error.contains("never return"));
    }
}

use super::*;
use rusqlite::params;
use tempfile::TempDir;
use wiremock::{
    matchers::{header, method, path},
    Mock, MockServer, ResponseTemplate,
};
fn setup(base: &str) -> (TempDir, AppState) {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path().join("test.db")).unwrap();
    for id in ["custom", "other"] {
        state
            .db()
            .unwrap()
            .execute(
                "INSERT OR IGNORE INTO providers(id,name,base_url,models_json) VALUES(?,?,?,'[]')",
                params![id, id, base],
            )
            .unwrap();
    }
    (dir, state)
}
async fn action(state: &AppState, value: Value) -> Result<(), String> {
    execute_action(
        state,
        serde_json::from_value(value).unwrap(),
        CancellationToken::new(),
        |_| Ok(()),
    )
    .await
}
#[tokio::test]
async fn complete_host_actions_validate_keys_and_persist_metadata_only() {
    let server = MockServer::start().await;
    Mock::given(path("/models"))
        .and(header("authorization", "Bearer valid-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[{"id":"model"}]})))
        .expect(2)
        .mount(&server)
        .await;
    let (_dir, state) = setup(&server.uri());
    for label in ["First", "Second"] {
        action(&state,json!({"type":"add-api-key","payload":{"providerId":"custom","label":label,"apiKey":"valid-key"}})).await.unwrap();
    }
    let items = connections::list(&state, "custom").unwrap();
    assert_eq!(items.len(), 2);
    assert_ne!(items[0].id, items[1].id);
    action(&state,json!({"type":"update-credential","payload":{"providerId":"custom","credentialId":items[0].id,"enabled":false,"weight":7}})).await.unwrap();
    action(&state,json!({"type":"update-strategy","payload":{"providerId":"custom","strategy":"weighted-round-robin"}})).await.unwrap();
    let result = auth_state(&state).unwrap();
    let encoded = serde_json::to_string(&result).unwrap();
    assert!(!encoded.contains("valid-key"));
    let p = result["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "custom")
        .unwrap();
    assert_eq!(p["loadStrategy"], "weighted-round-robin");
    assert_eq!(p["apiKeyCredentials"][0]["enabled"], false);
    assert_eq!(p["apiKeyCredentials"][0]["weight"], 7);
    action(&state,json!({"type":"remove-credential","providerId":"custom","credentialId":items[0].id,"authMethod":"api-key"})).await.unwrap();
    assert_eq!(connections::list(&state, "custom").unwrap().len(), 1);
}
#[tokio::test]
async fn invalid_key_or_wrong_provider_never_mutates_store() {
    let server = MockServer::start().await;
    Mock::given(path("/models"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({"error":"private-server-value"})),
        )
        .mount(&server)
        .await;
    let (dir, state) = setup(&server.uri());
    let error=action(&state,json!({"type":"add-api-key","payload":{"providerId":"custom","label":"Bad","apiKey":"wrong-secret"}})).await.unwrap_err();
    assert!(error.contains("401"));
    assert!(!error.contains("private-server"));
    assert!(connections::list(&state, "custom").unwrap().is_empty());
    assert!(!dir.path().join("credentials/credentials.json").exists());
    assert!(action(
        &state,
        json!({"type":"select-model","payload":{"providerId":"other","model":"absent"}})
    )
    .await
    .is_err());
    assert!(action(
        &state,
        json!({"type":"update-provider","payload":{"providerId":"custom","oauthEnabled":false}})
    )
    .await
    .is_err());
}
#[tokio::test]
async fn failover_uses_next_key_before_output_and_never_after_partial_output() {
    let server = MockServer::start().await;
    let (_dir, state) = setup(&server.uri());
    let a = Credential::new("custom", "api-key", "First".into(), vec!["model".into()]);
    let b = Credential::new("custom", "api-key", "Second".into(), vec!["model".into()]);
    connections::save(&state, &a, "first").unwrap();
    connections::save(&state, &b, "second").unwrap();
    connections::set_strategy(&state, "custom", "failover").unwrap();
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer first"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer second"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "data: {\"choices\":[{\"delta\":{\"content\":\"answer\"}}]}\n\ndata: [DONE]\n\n",
            "text/event-stream",
        ))
        .expect(1)
        .mount(&server)
        .await;
    let answer = stream_with(
        &state,
        "custom",
        "model",
        vec![json!({"role":"user","content":"question"})],
        CancellationToken::new(),
        |_| Ok(()),
    )
    .await
    .unwrap();
    assert_eq!(answer, "answer");
    assert!(!connections::get(&state, "custom", &a.id).unwrap().healthy);
    server.reset().await;
    connections::report_success(&state, "custom", &a.id).unwrap();
    Mock::given(path("/chat/completions"))
        .and(header("authorization", "Bearer first"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
            "text/event-stream",
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/chat/completions"))
        .and(header("authorization", "Bearer second"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    let chunks = std::sync::Mutex::new(Vec::new());
    assert!(stream_with(
        &state,
        "custom",
        "model",
        vec![json!({"role":"user","content":"question"})],
        CancellationToken::new(),
        |delta| {
            chunks.lock().unwrap().push(delta);
            Ok(())
        }
    )
    .await
    .is_err());
    assert_eq!(*chunks.lock().unwrap(), vec!["partial"]);
}
#[tokio::test]
async fn credential_requests_reject_cross_provider_and_disabled_credentials() {
    let (_dir, state) = setup("http://localhost");
    let item = Credential::new("custom", "api-key", "Name".into(), vec!["model".into()]);
    connections::save(&state, &item, "key").unwrap();
    assert!(
        credential_for_request(&state, "other", &item.id, CancellationToken::new())
            .await
            .is_err()
    );
    connections::update(&state, "custom", &item.id, false, 1).unwrap();
    assert!(
        credential_for_request(&state, "custom", &item.id, CancellationToken::new())
            .await
            .is_err()
    );
}
#[tokio::test]
async fn cancellation_before_key_verification_never_writes() {
    let server = MockServer::start().await;
    Mock::given(path("/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"data":[{"id":"model"}]}))
                .set_delay(std::time::Duration::from_secs(2)),
        )
        .mount(&server)
        .await;
    let (dir, state) = setup(&server.uri());
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        token.cancel();
    });
    let value = json!({"type":"add-api-key","payload":{"providerId":"custom","label":"Key","apiKey":"key"}});
    assert!(execute_action(
        &state,
        serde_json::from_value(value).unwrap(),
        cancel,
        |_| Ok(())
    )
    .await
    .is_err());
    assert!(connections::list(&state, "custom").unwrap().is_empty());
    assert!(!dir.path().join("credentials/credentials.json").exists());
}
#[test]
fn unreadable_secret_returns_auth_error_metadata_without_blocking_offline_state() {
    let (dir, state) = setup("http://localhost");
    let item = Credential::new("custom", "api-key", "Account".into(), vec!["model".into()]);
    connections::save(&state, &item, "key").unwrap();
    std::fs::write(dir.path().join("credentials/credentials.json"), "corrupt").unwrap();
    assert!(load_data(&state).is_ok());
    assert!(connections::has_credential(&state, "custom").unwrap());
    let result = auth_state(&state).unwrap();
    assert_eq!(result["catalogStatus"]["state"], "error");
    let p = result["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "custom")
        .unwrap();
    assert_eq!(p["apiKeyCredentials"][0]["healthy"], false);
    assert_eq!(p["apiKeyCredentials"][0]["models"], json!(["model"]));
}

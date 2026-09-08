use super::*;
use serde_json::json;
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn cancellation_prevents_network_and_browser() {
    let cancel = CancellationToken::new();
    cancel.cancel();
    for id in ["workbuddy", "traecode"] {
        let result = authorize(id, cancel.clone(), |_| {
            panic!("cancelled operation opened browser")
        })
        .await;
        assert!(result.unwrap_err().contains("cancelled"));
        assert!(refresh(id, &json!({}), cancel.clone())
            .await
            .unwrap_err()
            .contains("cancelled"));
    }
}
#[tokio::test]
async fn cancelling_trae_closes_callback_listener() {
    let cancel = CancellationToken::new();
    let capture = Arc::new(Mutex::new(String::new()));
    let target = capture.clone();
    let on_cancel = cancel.clone();
    let result = authorize("traecode", cancel, move |url| {
        let url = url::Url::parse(&url).unwrap();
        assert_eq!(url.scheme(), "https");
        assert!(url
            .query_pairs()
            .any(|(k, v)| k == "code_challenge_method" && v == "S256"));
        *target.lock().unwrap() = url
            .query_pairs()
            .find(|(k, _)| k == "auth_callback_url")
            .unwrap()
            .1
            .into_owned();
        on_cancel.cancel();
        Ok(())
    })
    .await;
    assert!(result.unwrap_err().contains("cancelled"));
    tokio::task::yield_now().await;
    let callback = capture.lock().unwrap().clone();
    assert!(reqwest::get(callback).await.is_err());
}
#[test]
fn rejects_tool_history_and_malformed_messages() {
    assert!(validate_messages(&[]).is_err());
    assert!(validate_messages(&[json!({"role":"tool","content":"text"})]).is_err());
    assert!(validate_messages(&[json!({"role":"user","content":[]})]).is_err());
    assert!(
        validate_messages(&[json!({"role":"assistant","content":"","tool_calls":[]})]).is_err()
    );
    assert_eq!(
        validate_messages(&[json!({"role":"user","content":"你好"})])
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn refresh_deadline_is_provider_specific() {
    let later = chrono::Utc::now().timestamp_millis() + 120_000;
    assert!(!needs_refresh("traecode", &json!({"expires":later})));
    assert!(!needs_refresh("workbuddy", &json!({"expires_at":later})));
    assert!(needs_refresh("traecode", &json!({})));
}

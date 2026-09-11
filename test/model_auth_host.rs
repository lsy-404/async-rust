use super::*;
use rusqlite::params;
use std::sync::atomic::AtomicUsize;
use tempfile::TempDir;
use wiremock::{
    matchers::{header, method, path},
    Mock, MockServer, Request, ResponseTemplate,
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
        StreamOptions {
            tool_workspace_id: None,
            cancel: CancellationToken::new(),
        },
        |_| Ok(()),
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
        StreamOptions {
            tool_workspace_id: None,
            cancel: CancellationToken::new(),
        },
        |delta| {
            chunks.lock().unwrap().push(delta);
            Ok(())
        },
        |_| Ok(()),
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
    assert!(connections::has_credential_db(&state.db().unwrap(), "custom").unwrap());
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
fn seed_workspace(state: &AppState, id: &str) {
    state
        .db()
        .unwrap()
        .execute(
            "INSERT INTO workspaces(id,name) VALUES(?,?)",
            params![id, id],
        )
        .unwrap();
}
fn sse_body(events: &[Value]) -> String {
    let mut out = String::new();
    for event in events {
        out.push_str(&format!("data: {event}\n\n"));
    }
    out.push_str("data: [DONE]\n\n");
    out
}
// Builds one SSE chunk carrying a tool-call delta fragment, mirroring the
// shape a real OpenAI-compatible provider streams: `id`/`name` typically
// arrive once and `arguments` accumulates across several such fragments.
fn tool_call_chunk(index: u64, id: Option<&str>, name: Option<&str>, arguments: &str) -> Value {
    let mut function = json!({"arguments": arguments});
    if let Some(name) = name {
        function["name"] = json!(name);
    }
    let mut call = json!({"index": index, "function": function});
    if let Some(id) = id {
        call["id"] = json!(id);
    }
    json!({"choices": [{"index": 0, "delta": {"tool_calls": [call]}}]})
}
fn content_chunk(text: &str) -> Value {
    json!({"choices": [{"index": 0, "delta": {"content": text}}]})
}
#[tokio::test]
async fn tool_call_arguments_accumulate_across_streamed_chunks_before_the_tool_runs() {
    let server = MockServer::start().await;
    let (_dir, state) = setup(&server.uri());
    seed_workspace(&state, "ws-accum");
    state
        .db()
        .unwrap()
        .execute(
            "INSERT INTO materials(id,workspace_id,name,content) VALUES(?,?,?,?)",
            params![
                "m1",
                "ws-accum",
                "notes.txt",
                "Algebra is fun and useful."
            ],
        )
        .unwrap();
    let cred = Credential::new("custom", "api-key", "Key".into(), vec!["model".into()]);
    connections::save(&state, &cred, "key").unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(move |_req: &Request| {
            let round = counter.fetch_add(1, Ordering::SeqCst);
            let body = if round == 0 {
                // The id and the function name arrive once; the JSON arguments are
                // split across two separate chunks and must be concatenated in order.
                sse_body(&[
                    tool_call_chunk(0, Some("call_abc"), Some("search_local_materials"), "{\"que"),
                    tool_call_chunk(0, None, None, "ry\":\"algebra\"}"),
                ])
            } else {
                sse_body(&[content_chunk("Found it.")])
            };
            ResponseTemplate::new(200).set_body_raw(body, "text/event-stream")
        })
        .expect(2)
        .mount(&server)
        .await;
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = events.clone();
    let answer = stream_with(
        &state,
        "custom",
        "model",
        vec![json!({"role":"user","content":"question"})],
        StreamOptions {
            tool_workspace_id: Some("ws-accum"),
            cancel: CancellationToken::new(),
        },
        |_| Ok(()),
        move |event: StreamEvent| {
            sink.lock().unwrap().push(event);
            Ok(())
        },
    )
    .await
    .unwrap();
    assert_eq!(answer, "Found it.");
    let events = events.lock().unwrap();
    let requested = events
        .iter()
        .find(|event| event.tool_status.as_deref() == Some("requested"))
        .expect("a requested event");
    assert_eq!(
        requested.tool_name.as_deref(),
        Some("search_local_materials")
    );
    assert_eq!(
        requested.tool_arguments.as_deref(),
        Some("{\"query\":\"algebra\"}")
    );
    let finished = events
        .iter()
        .find(|event| event.tool_status.as_deref() == Some("finished"))
        .expect("a finished event");
    assert!(finished
        .tool_result
        .as_deref()
        .unwrap()
        .contains("Algebra is fun"));
}
#[tokio::test]
async fn provider_without_tool_support_falls_back_and_still_completes_the_turn() {
    // A provider that doesn't support tool calling rejects the request outright
    // (HTTP 400) whenever the tools array is present; the very first, still-empty
    // attempt should retry once without it rather than failing the whole turn.
    let server = MockServer::start().await;
    let (_dir, state) = setup(&server.uri());
    seed_workspace(&state, "ws-fallback");
    let cred = Credential::new("custom", "api-key", "Key".into(), vec!["model".into()]);
    connections::save(&state, &cred, "key").unwrap();
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(|req: &Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            if body.get("tools").is_some() {
                ResponseTemplate::new(400)
            } else {
                ResponseTemplate::new(200)
                    .set_body_raw(sse_body(&[content_chunk("plain answer")]), "text/event-stream")
            }
        })
        .expect(2)
        .mount(&server)
        .await;
    let answer = stream_with(
        &state,
        "custom",
        "model",
        vec![json!({"role":"user","content":"question"})],
        StreamOptions {
            tool_workspace_id: Some("ws-fallback"),
            cancel: CancellationToken::new(),
        },
        |_| Ok(()),
        |_| Ok(()),
    )
    .await
    .unwrap();
    assert_eq!(answer, "plain answer");
}
#[tokio::test]
async fn tool_loop_stops_after_max_rounds_and_finishes_without_tools() {
    let server = MockServer::start().await;
    let (_dir, state) = setup(&server.uri());
    seed_workspace(&state, "ws-bound");
    let cred = Credential::new("custom", "api-key", "Key".into(), vec!["model".into()]);
    connections::save(&state, &cred, "key").unwrap();
    let had_tools = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = had_tools.clone();
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(move |req: &Request| {
            let request_body: Value = serde_json::from_slice(&req.body).unwrap();
            let mut seen = seen.lock().unwrap();
            seen.push(request_body.get("tools").is_some());
            let round = seen.len();
            drop(seen);
            let body = if round <= MAX_TOOL_ROUNDS {
                let id = format!("call_{round}");
                sse_body(&[tool_call_chunk(
                    0,
                    Some(&id),
                    Some("search_local_materials"),
                    "{\"query\":\"x\"}",
                )])
            } else {
                sse_body(&[content_chunk("final answer")])
            };
            ResponseTemplate::new(200).set_body_raw(body, "text/event-stream")
        })
        .expect(MAX_TOOL_ROUNDS as u64 + 1)
        .mount(&server)
        .await;
    let answer = stream_with(
        &state,
        "custom",
        "model",
        vec![json!({"role":"user","content":"question"})],
        StreamOptions {
            tool_workspace_id: Some("ws-bound"),
            cancel: CancellationToken::new(),
        },
        |_| Ok(()),
        |_| Ok(()),
    )
    .await
    .unwrap();
    assert_eq!(answer, "final answer");
    let seen = had_tools.lock().unwrap();
    assert_eq!(seen.len(), MAX_TOOL_ROUNDS + 1);
    assert!(
        seen[..MAX_TOOL_ROUNDS].iter().all(|flag| *flag),
        "every round within budget must still offer the tool: {seen:?}"
    );
    assert!(
        !seen[MAX_TOOL_ROUNDS],
        "the final pass must omit the tools array so the turn can end: {seen:?}"
    );
}
#[test]
fn search_local_finds_real_rows_from_materials_and_session_transcripts() {
    let (_dir, state) = setup("http://localhost");
    seed_workspace(&state, "ws-search");
    seed_workspace(&state, "ws-other");
    let db = state.db().unwrap();
    db.execute(
        "INSERT INTO materials(id,workspace_id,name,content) VALUES(?,?,?,?)",
        params![
            "m1",
            "ws-search",
            "notes.txt",
            "The mitochondria is the powerhouse of the cell."
        ],
    )
    .unwrap();
    db.execute(
        "INSERT INTO sessions(id,workspace_id,title,transcription) VALUES(?,?,?,?)",
        params![
            "s1",
            "ws-search",
            "Lecture",
            "Today we discussed the mitochondria in detail."
        ],
    )
    .unwrap();
    // A matching row that belongs to a different workspace must never leak in.
    db.execute(
        "INSERT INTO materials(id,workspace_id,name,content) VALUES(?,?,?,?)",
        params!["m2", "ws-other", "other.txt", "mitochondria appears here too"],
    )
    .unwrap();
    drop(db);
    let result = search_local(&state, "ws-search", "mitochondria").unwrap();
    assert!(result.contains("[Material] notes.txt"));
    assert!(result.contains("[Session transcript] Lecture"));
    assert!(!result.contains("other.txt"));
}
#[test]
fn escape_like_escapes_backslash_percent_and_underscore_in_that_order() {
    assert_eq!(escape_like("50%_off"), "50\\%\\_off");
    assert_eq!(escape_like("a\\b"), "a\\\\b");
}
#[test]
fn like_escaping_prevents_percent_and_underscore_from_matching_everything() {
    let (_dir, state) = setup("http://localhost");
    seed_workspace(&state, "ws-escape");
    let db = state.db().unwrap();
    db.execute(
        "INSERT INTO materials(id,workspace_id,name,content) VALUES(?,?,?,?)",
        params![
            "m1",
            "ws-escape",
            "notes.txt",
            "Plain notes without special characters."
        ],
    )
    .unwrap();
    db.execute(
        "INSERT INTO materials(id,workspace_id,name,content) VALUES(?,?,?,?)",
        params![
            "m2",
            "ws-escape",
            "other.txt",
            "Another plain note, nothing special."
        ],
    )
    .unwrap();
    drop(db);
    // Neither material contains a literal '%' or '_'; if either were treated as an
    // unescaped SQL wildcard it would match both rows instead of finding nothing.
    assert!(search_local(&state, "ws-escape", "%")
        .unwrap()
        .contains("No local materials"));
    assert!(search_local(&state, "ws-escape", "_")
        .unwrap()
        .contains("No local materials"));
    state
        .db()
        .unwrap()
        .execute(
            "INSERT INTO materials(id,workspace_id,name,content) VALUES(?,?,?,?)",
            params![
                "m3",
                "ws-escape",
                "promo.txt",
                "Save 100%_off today only."
            ],
        )
        .unwrap();
    // A query that legitimately contains those characters must still find the
    // literal text verbatim, and only the material that actually has it.
    let literal = search_local(&state, "ws-escape", "100%_off").unwrap();
    assert!(literal.contains("promo.txt"));
    assert!(!literal.contains("notes.txt"));
    assert!(!literal.contains("other.txt"));
}

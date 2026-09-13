use super::*;
use std::sync::Arc;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

// Shared by the notes tests below: a workspace, a session, a provider row
// pointed at the wiremock server, matching settings, and a healthy credential.
fn seed_notes_fixture(state: &AppState, server_uri: &str) {
    let db = state.db().unwrap();
    db.execute("INSERT INTO workspaces VALUES('w','Class')", [])
        .unwrap();
    db.execute(
        "INSERT INTO sessions(id,workspace_id,title) VALUES('s','w','Lesson')",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT OR IGNORE INTO providers(id,name,base_url,models_json) VALUES('custom','custom',?,'[]')",
        [server_uri],
    )
    .unwrap();
    let settings = Settings {
        provider_id: "custom".into(),
        model: "model".into(),
        ..Settings::default()
    };
    db.execute(
        "INSERT INTO settings(singleton,json) VALUES(1,?) ON CONFLICT(singleton) DO UPDATE SET json=excluded.json",
        [serde_json::to_string(&settings).unwrap()],
    )
    .unwrap();
    drop(db);
    connections::save(
        state,
        &connections::Credential::new("custom", "api-key", "Key".into(), vec!["model".into()]),
        "test-key",
    )
    .unwrap();
}
fn sse_answer(text: &str) -> String {
    format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\"}}}}]}}\n\ndata: [DONE]\n\n")
}

#[test]
fn sqlite_cascade_survives_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.sqlite3");
    {
        let db = open_database(&path).unwrap();
        db.execute("INSERT INTO workspaces VALUES('w','Class')", [])
            .unwrap();
        db.execute(
            "INSERT INTO sessions(id,workspace_id,title) VALUES('s','w','Lesson')",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO materials VALUES('m','w','n','content',NULL)",
            [],
        )
        .unwrap();
    }
    let db = open_database(&path).unwrap();
    db.execute("DELETE FROM workspaces WHERE id='w'", [])
        .unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM materials", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn rename_workspace_updates_name_and_rejects_blank() {
    let temp = tempfile::tempdir().unwrap();
    let state = AppState::open(temp.path().join("state.sqlite3")).unwrap();
    state
        .db()
        .unwrap()
        .execute("INSERT INTO workspaces VALUES('w','Class')", [])
        .unwrap();

    rename_workspace_impl(&state, "w", "Renamed").unwrap();
    let name: String = state
        .db()
        .unwrap()
        .query_row("SELECT name FROM workspaces WHERE id='w'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(name, "Renamed");

    let err = rename_workspace_impl(&state, "w", "   ").unwrap_err();
    assert_eq!(err, "名称不能为空。");

    let err = rename_workspace_impl(&state, "missing", "x").unwrap_err();
    assert_eq!(err, "找不到工作区。");
}

#[test]
fn summary_timestamp_survives_reload() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.sqlite3");
    let db = open_database(&path).unwrap();
    db.execute("INSERT INTO workspaces VALUES('w','Class')", [])
        .unwrap();
    db.execute(
        "INSERT INTO sessions(id,workspace_id,title) VALUES('s','w','Lesson')",
        [],
    )
    .unwrap();
    db.execute(
        "UPDATE sessions SET summary='done',summary_updated_at='2024-01-01T00:00:00Z' WHERE id='s'",
        [],
    )
    .unwrap();

    let state = AppState::open(path).unwrap();
    let data = load_data(&state).unwrap();
    let session = data.sessions.iter().find(|s| s.id == "s").unwrap();
    assert_eq!(session.summary.as_deref(), Some("done"));
    assert_eq!(
        session.summary_updated_at.as_deref(),
        Some("2024-01-01T00:00:00Z")
    );
}

#[test]
fn rename_session_updates_title_and_rejects_blank() {
    let temp = tempfile::tempdir().unwrap();
    let state = AppState::open(temp.path().join("state.sqlite3")).unwrap();
    state
        .db()
        .unwrap()
        .execute("INSERT INTO workspaces VALUES('w','Class')", [])
        .unwrap();
    state
        .db()
        .unwrap()
        .execute(
            "INSERT INTO sessions(id,workspace_id,title) VALUES('s','w','Lesson')",
            [],
        )
        .unwrap();

    rename_session_impl(&state, "s", "Renamed").unwrap();
    let title: String = state
        .db()
        .unwrap()
        .query_row("SELECT title FROM sessions WHERE id='s'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(title, "Renamed");

    let err = rename_session_impl(&state, "s", "   ").unwrap_err();
    assert_eq!(err, "名称不能为空。");

    let err = rename_session_impl(&state, "missing", "x").unwrap_err();
    assert_eq!(err, "找不到会话。");
}

#[tokio::test]
async fn chat_rejects_concurrent_generation_without_persisting_user_message() {
    let temp = tempfile::tempdir().unwrap();
    let state = AppState::open(temp.path().join("state.sqlite3")).unwrap();
    state
        .db()
        .unwrap()
        .execute("INSERT INTO workspaces VALUES('w','Class')", [])
        .unwrap();
    state
        .db()
        .unwrap()
        .execute(
            "INSERT INTO sessions(id,workspace_id,title) VALUES('s','w','Lesson')",
            [],
        )
        .unwrap();

    // Simulate a generation already in flight for this session.
    let _held_token = generation_token(&state, "s").unwrap();

    let channel = Channel::<StreamEvent>::new(|_| Ok(()));
    let result = chat_impl(&state, "s", "hello", channel).await;

    assert_eq!(result.unwrap_err(), "该会话已有生成任务。");
    let message_count: i64 = state
        .db()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id='s'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(message_count, 0);
}

#[test]
fn preexisting_database_without_summary_column_does_not_crash() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.sqlite3");
    {
        let legacy = Connection::open(&path).unwrap();
        legacy
            .execute_batch(
                "CREATE TABLE workspaces(id TEXT PRIMARY KEY, name TEXT NOT NULL);
                 CREATE TABLE sessions(id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, title TEXT NOT NULL, transcription TEXT, summary TEXT);",
            )
            .unwrap();
    }
    // Reopening through the app's own schema setup must not fail on the legacy table shape.
    let db = open_database(&path).unwrap();
    db.execute(
        "UPDATE sessions SET summary_updated_at='now' WHERE id='missing'",
        [],
    )
    .unwrap();
}

#[test]
fn created_at_round_trips_through_save_session_edits() {
    let temp = tempfile::tempdir().unwrap();
    let state = AppState::open(temp.path().join("state.sqlite3")).unwrap();
    let db = state.db().unwrap();
    db.execute("INSERT INTO workspaces VALUES('w','Class')", [])
        .unwrap();
    db.execute(
        "INSERT INTO sessions(id,workspace_id,title) VALUES('s','w','Lesson')",
        [],
    )
    .unwrap();
    drop(db);

    // A brand-new message with no created_at gets stamped on its first save.
    let first = Session {
        id: "s".into(),
        workspace_id: "w".into(),
        title: "Lesson".into(),
        messages: vec![Message {
            id: "m1".into(),
            role: "user".into(),
            content: "hello".into(),
            created_at: String::new(),
            tool_calls: None,
        }],
        transcription: None,
        summary: None,
        summary_updated_at: None,
        transcription_words: None,
        notes_enabled: true,
    };
    save_session_with(first, &state).unwrap();
    let loaded = load_data(&state).unwrap();
    let session = loaded.sessions.iter().find(|s| s.id == "s").unwrap();
    let stamped = session.messages[0].created_at.clone();
    assert!(!stamped.is_empty(), "expected a created_at to be assigned");

    // Editing the session (adding a second message) must not disturb the
    // first message's created_at, even though save_session deletes and
    // reinserts every row.
    let mut edited = session.clone();
    edited.messages.push(Message {
        id: "m2".into(),
        role: "assistant".into(),
        content: "hi".into(),
        created_at: String::new(),
        tool_calls: None,
    });
    save_session_with(edited, &state).unwrap();
    let reloaded = load_data(&state).unwrap();
    let session = reloaded.sessions.iter().find(|s| s.id == "s").unwrap();
    assert_eq!(session.messages[0].created_at, stamped);
    assert!(!session.messages[1].created_at.is_empty());
    assert_ne!(
        session.messages[0].created_at,
        session.messages[1].created_at.clone()
    );
}

#[test]
fn tool_calls_persist_and_survive_a_reload() {
    let temp = tempfile::tempdir().unwrap();
    let state = AppState::open(temp.path().join("state.sqlite3")).unwrap();
    let db = state.db().unwrap();
    db.execute("INSERT INTO workspaces VALUES('w','Class')", [])
        .unwrap();
    db.execute(
        "INSERT INTO sessions(id,workspace_id,title) VALUES('s','w','Lesson')",
        [],
    )
    .unwrap();
    drop(db);

    let calls = vec![ToolCall {
        id: "call_1".into(),
        name: "search_local_materials".into(),
        status: "finished".into(),
        arguments: Some("{\"query\":\"algebra\"}".into()),
        result: Some("Algebra is fun.".into()),
    }];
    persist_message(
        &state,
        "s",
        "assistant",
        "Here is what I found.",
        Some(&calls),
    )
    .unwrap();

    // The card must still be there after the same reload the frontend does
    // right after a turn finishes (load_state), not just in the live stream.
    let loaded = load_data(&state).unwrap();
    let session = loaded.sessions.iter().find(|s| s.id == "s").unwrap();
    let tool_calls = session.messages[0]
        .tool_calls
        .as_ref()
        .expect("tool calls must survive a reload");
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0].id, "call_1");
    assert_eq!(tool_calls[0].result.as_deref(), Some("Algebra is fun."));

    // save_session's delete-and-reinsert (an edit/regenerate) must carry
    // tool_calls through untouched, the same way created_at already does.
    save_session_with(session.clone(), &state).unwrap();
    let reloaded = load_data(&state).unwrap();
    let session = reloaded.sessions.iter().find(|s| s.id == "s").unwrap();
    let tool_calls = session.messages[0]
        .tool_calls
        .as_ref()
        .expect("tool calls must survive save_session's reinsert");
    assert_eq!(tool_calls[0].id, "call_1");
}

#[test]
fn legacy_messages_get_an_empty_created_at_not_a_crash() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.sqlite3");
    {
        // Simulate a pre-existing database from before the created_at column existed.
        let db = open_database(&path).unwrap();
        db.execute("ALTER TABLE messages DROP COLUMN created_at", [])
            .unwrap();
        db.execute("INSERT INTO workspaces VALUES('w','Class')", [])
            .unwrap();
        db.execute(
            "INSERT INTO sessions(id,workspace_id,title) VALUES('s','w','Lesson')",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO messages(id,session_id,role,content,position) VALUES('m','s','user','hi',0)",
            [],
        )
        .unwrap();
    }
    // Reopening must not crash, and must additively repair the missing column.
    let state = AppState::open(path).unwrap();
    let data = load_data(&state).unwrap();
    let session = data.sessions.iter().find(|s| s.id == "s").unwrap();
    assert_eq!(session.messages[0].created_at, "");
}

// Trigger rule: once the transcript has grown by NOTES_THRESHOLD_CHARS (800)
// past the last covered point, the next chunk sent carries NOTES_OVERLAP_CHARS
// (200) of context from before that point plus everything new, and the cursor
// advances to (new length - 200) so the next round's overlap comes from here.
#[test]
fn next_notes_chunk_fires_at_800_new_chars_and_carries_200_overlap() {
    let below = "x".repeat(NOTES_THRESHOLD_CHARS - 1);
    assert!(next_notes_chunk(&below, 0).is_none());

    let a = "A".repeat(1000);
    let b = "B".repeat(NOTES_THRESHOLD_CHARS + 600);
    let transcript = format!("{a}{b}");
    let cursor = a.len();
    // Not yet 800 new characters past the cursor.
    assert!(next_notes_chunk(&transcript[..cursor + NOTES_THRESHOLD_CHARS - 1], cursor).is_none());

    let (chunk, new_cursor) = next_notes_chunk(&transcript, cursor).unwrap();
    assert!(chunk.starts_with(&"A".repeat(NOTES_OVERLAP_CHARS)));
    assert!(!chunk.starts_with(&"A".repeat(NOTES_OVERLAP_CHARS + 1)));
    assert!(chunk.ends_with(&b));
    assert_eq!(chunk.len(), NOTES_OVERLAP_CHARS + b.len());
    assert_eq!(new_cursor, transcript.len() - NOTES_OVERLAP_CHARS);
    // No further growth yet: the next round must not fire again immediately.
    assert!(next_notes_chunk(&transcript, new_cursor).is_none());
}

#[tokio::test]
async fn background_notes_send_incremental_requests_and_persist_across_reload() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.sqlite3");
    let state = AppState::open(db_path.clone()).unwrap();
    seed_notes_fixture(&state, &server.uri());

    let bodies: Arc<std::sync::Mutex<Vec<Value>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = bodies.clone();
    let answers = Arc::new(std::sync::Mutex::new(vec!["Round two.", "Round one."]));
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(move |req: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            seen.lock().unwrap().push(body);
            let text = answers.lock().unwrap().pop().unwrap();
            ResponseTemplate::new(200).set_body_raw(sse_answer(text), "text/event-stream")
        })
        .mount(&server)
        .await;

    // Below threshold: no background round fires.
    append_transcription(&state, "s", &"x".repeat(NOTES_THRESHOLD_CHARS - 1)).unwrap();
    on_transcript_appended(&state, "s", &|_| {}).await;
    assert!(bodies.lock().unwrap().is_empty());

    // Crossing the threshold fires the first round with no prior notes to carry.
    append_transcription(&state, "s", &"y".repeat(50)).unwrap();
    on_transcript_appended(&state, "s", &|_| {}).await;
    {
        let seen = bodies.lock().unwrap();
        assert_eq!(seen.len(), 1);
        let content = seen[0]["messages"][1]["content"].as_str().unwrap();
        assert!(!content.contains("Notes so far"));
        assert!(content.contains("New transcript excerpt"));
    }
    let (session, _, _) = session_context(&state, "s").unwrap();
    assert_eq!(session.summary.as_deref(), Some("Round one."));
    assert!(session.summary_updated_at.is_some());

    // A second threshold crossing sends the existing notes plus only the new
    // excerpt, not the whole transcript again.
    append_transcription(&state, "s", &"z".repeat(NOTES_THRESHOLD_CHARS + 50)).unwrap();
    on_transcript_appended(&state, "s", &|_| {}).await;
    {
        let seen = bodies.lock().unwrap();
        assert_eq!(seen.len(), 2);
        let content = seen[1]["messages"][1]["content"].as_str().unwrap();
        assert!(content.contains("Notes so far:\nRound one."));
        assert!(content.contains("New transcript excerpt"));
    }

    // Not enough new growth yet: no third round.
    append_transcription(&state, "s", &"w".repeat(10)).unwrap();
    on_transcript_appended(&state, "s", &|_| {}).await;
    assert_eq!(bodies.lock().unwrap().len(), 2);

    // Persisted notes and their timestamp survive a reload.
    drop(state);
    let reopened = AppState::open(db_path).unwrap();
    let data = load_data(&reopened).unwrap();
    let session = data.sessions.iter().find(|s| s.id == "s").unwrap();
    assert_eq!(session.summary.as_deref(), Some("Round two."));
    assert!(session.summary_updated_at.is_some());
}

#[tokio::test]
async fn notes_job_defers_while_chat_or_summarize_holds_the_session_and_retries_once_free() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path().join("state.sqlite3")).unwrap();
    seed_notes_fixture(&state, &server.uri());
    append_transcription(&state, "s", &"z".repeat(NOTES_THRESHOLD_CHARS + 10)).unwrap();

    // Chat (or an explicit summarize) is using the session's ordinary generation slot.
    let held = generation_token(&state, "s").unwrap();
    on_transcript_appended(&state, "s", &|_| {}).await;
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "a notes round must never race chat/summarize over the shared summary column"
    );
    drop(held);

    // The slot is free again; the deferred round now runs.
    state.cancellations.lock().unwrap().remove("s");
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(sse_answer("Notes."), "text/event-stream"),
        )
        .expect(1)
        .mount(&server)
        .await;
    on_transcript_appended(&state, "s", &|_| {}).await;
    let (session, _, _) = session_context(&state, "s").unwrap();
    assert_eq!(session.summary.as_deref(), Some("Notes."));
}

#[tokio::test]
async fn chat_is_not_blocked_by_an_in_flight_notes_job_for_the_same_session() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path().join("state.sqlite3")).unwrap();
    seed_notes_fixture(&state, &server.uri());

    // A background notes job is in flight: it holds the *notes* slot, never chat's.
    let _notes_token = notes_generation_token(&state, "s").unwrap();

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(sse_answer("answer"), "text/event-stream"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let channel = Channel::<StreamEvent>::new(|_| Ok(()));
    chat_impl(&state, "s", "hello", channel).await.unwrap();

    let message_count: i64 = state
        .db()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id='s'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        message_count, 2,
        "chat must complete normally, unblocked by the notes job"
    );
}

#[test]
fn stop_notes_for_session_cancels_the_token_and_clears_tracking() {
    let temp = tempfile::tempdir().unwrap();
    let state = AppState::open(temp.path().join("state.sqlite3")).unwrap();
    state
        .db()
        .unwrap()
        .execute("INSERT INTO workspaces VALUES('w','Class')", [])
        .unwrap();
    state
        .db()
        .unwrap()
        .execute(
            "INSERT INTO sessions(id,workspace_id,title) VALUES('s','w','Lesson')",
            [],
        )
        .unwrap();

    let token = notes_generation_token(&state, "s").unwrap();
    state.notes.lock().unwrap().insert(
        "s".into(),
        NotesTracker {
            cursor: 42,
            running: true,
            pending: true,
        },
    );

    stop_notes_for_session(&state, "s");

    assert!(token.is_cancelled());
    assert!(state.notes.lock().unwrap().get("s").is_none());
    assert!(state.notes_cancellations.lock().unwrap().get("s").is_none());
    // Not sticky: a fresh job can be reserved again right away.
    assert!(notes_generation_token(&state, "s").is_ok());
}

#[test]
fn set_notes_enabled_persists_the_flag_and_stops_a_running_job_when_disabled() {
    let temp = tempfile::tempdir().unwrap();
    let state = AppState::open(temp.path().join("state.sqlite3")).unwrap();
    state
        .db()
        .unwrap()
        .execute("INSERT INTO workspaces VALUES('w','Class')", [])
        .unwrap();
    state
        .db()
        .unwrap()
        .execute(
            "INSERT INTO sessions(id,workspace_id,title) VALUES('s','w','Lesson')",
            [],
        )
        .unwrap();
    let token = notes_generation_token(&state, "s").unwrap();

    set_notes_enabled_impl(&state, "s", false).unwrap();

    assert!(token.is_cancelled());
    let enabled: bool = state
        .db()
        .unwrap()
        .query_row("SELECT notes_enabled FROM sessions WHERE id='s'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(!enabled);

    let err = set_notes_enabled_impl(&state, "missing", true).unwrap_err();
    assert_eq!(err, "找不到会话。");
}

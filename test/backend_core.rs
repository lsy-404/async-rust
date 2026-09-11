use super::*;

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
    persist_message(&state, "s", "assistant", "Here is what I found.", Some(&calls)).unwrap();

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

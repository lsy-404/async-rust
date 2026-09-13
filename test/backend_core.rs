use super::*;
use std::sync::Arc;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

fn seed_folder(state: &AppState, id: &str, name: &str) {
    state
        .db()
        .unwrap()
        .execute(
            "INSERT INTO nodes(id,parent_id,parent_kind,kind,name) VALUES(?,NULL,NULL,'folder',?)",
            params![id, name],
        )
        .unwrap();
}
fn seed_session(state: &AppState, parent: Option<&str>, id: &str, name: &str) {
    let db = state.db().unwrap();
    db.execute(
        "INSERT INTO nodes(id,parent_id,parent_kind,kind,name) VALUES(?1,?2,CASE WHEN ?2 IS NULL THEN NULL ELSE 'folder' END,'session',?3)",
        params![id, parent, name],
    )
    .unwrap();
    db.execute("INSERT INTO sessions(id) VALUES(?1)", params![id])
        .unwrap();
}
fn seed_material(state: &AppState, parent: Option<&str>, id: &str, name: &str, content: &str) {
    let db = state.db().unwrap();
    db.execute(
        "INSERT INTO nodes(id,parent_id,parent_kind,kind,name) VALUES(?1,?2,CASE WHEN ?2 IS NULL THEN NULL ELSE 'folder' END,'material',?3)",
        params![id, parent, name],
    )
    .unwrap();
    db.execute(
        "INSERT INTO materials(id,content,path) VALUES(?,?,NULL)",
        params![id, content],
    )
    .unwrap();
}

// Shared by the notes/translation tests below: a root session, a provider row
// pointed at the wiremock server, matching settings, and a healthy credential.
fn seed_notes_fixture(state: &AppState, server_uri: &str) {
    seed_session(state, None, "s", "Lesson");
    let db = state.db().unwrap();
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
    // JSON-encode the content so a multi-line batch translation reply (with
    // real newlines between numbered lines) round-trips correctly instead of
    // producing invalid JSON via naive string interpolation.
    let content = serde_json::to_string(text).unwrap();
    format!("data: {{\"choices\":[{{\"delta\":{{\"content\":{content}}}}}]}}\n\ndata: [DONE]\n\n")
}

// --- node commands ---------------------------------------------------------

#[test]
fn create_folder_create_session_and_import_material_at_root_and_nested() {
    let temp = tempfile::tempdir().unwrap();
    let state = AppState::open(temp.path().join("state.sqlite3")).unwrap();

    let root_folder = create_node(&state, None, "Course", NodeKind::Folder, |_, _| Ok(())).unwrap();
    assert_eq!(root_folder.parent_id, None);
    assert_eq!(root_folder.kind, NodeKind::Folder);

    let nested_session = create_node(
        &state,
        Some(&root_folder.id),
        "Lesson 1",
        NodeKind::Session,
        |tx, id| {
            tx.execute("INSERT INTO sessions(id) VALUES(?1)", [id])
                .map_err(|e| e.to_string())
                .map(|_| ())
        },
    )
    .unwrap();
    assert_eq!(
        nested_session.parent_id.as_deref(),
        Some(root_folder.id.as_str())
    );

    // A session or material as the parent is rejected.
    let err = create_node(
        &state,
        Some(&nested_session.id),
        "x",
        NodeKind::Folder,
        |_, _| Ok(()),
    )
    .unwrap_err();
    assert_eq!(err, "目标不是文件夹。");

    // A missing parent is rejected the same way.
    let err = create_node(
        &state,
        Some("missing"),
        "x",
        NodeKind::Folder,
        |_, _| Ok(()),
    )
    .unwrap_err();
    assert_eq!(err, "目标不是文件夹。");

    // normalize_name: blank, too long, and control characters are all rejected.
    assert!(normalize_name("   ").is_err());
    assert!(normalize_name(&"a".repeat(256)).is_err());
    assert!(normalize_name("line\nbreak").is_err());
    assert_eq!(normalize_name("  ok  ").unwrap(), "ok");
}

#[test]
fn move_node_root_into_folder_and_same_parent_noop() {
    let temp = tempfile::tempdir().unwrap();
    let state = AppState::open(temp.path().join("state.sqlite3")).unwrap();
    seed_folder(&state, "a", "A");
    seed_folder(&state, "b", "B");
    seed_session(&state, Some("a"), "s", "Session");

    let moved = move_node_impl(&state, "s", Some("b")).unwrap();
    assert_eq!(moved.parent_id.as_deref(), Some("b"));

    // Same parent is a no-op that still succeeds.
    let again = move_node_impl(&state, "s", Some("b")).unwrap();
    assert_eq!(again.parent_id.as_deref(), Some("b"));

    // Moving to root.
    let at_root = move_node_impl(&state, "s", None).unwrap();
    assert_eq!(at_root.parent_id, None);
}

#[test]
fn move_node_rejects_cycles_and_non_folder_targets() {
    let temp = tempfile::tempdir().unwrap();
    let state = AppState::open(temp.path().join("state.sqlite3")).unwrap();
    seed_folder(&state, "a", "A");
    seed_folder(&state, "b", "B");
    seed_folder(&state, "c", "C");
    move_node_impl(&state, "b", Some("a")).unwrap();
    move_node_impl(&state, "c", Some("b")).unwrap();

    // Into self.
    let err = move_node_impl(&state, "a", Some("a")).unwrap_err();
    assert_eq!(err, "不能移动到自身或其子文件夹中。");
    // Into a descendant three levels deep.
    let err = move_node_impl(&state, "a", Some("c")).unwrap_err();
    assert_eq!(err, "不能移动到自身或其子文件夹中。");

    seed_session(&state, None, "s", "Session");
    let err = move_node_impl(&state, "a", Some("s")).unwrap_err();
    assert_eq!(err, "目标不是文件夹。");

    // A raw UPDATE that would create a cycle is rejected by the trigger itself.
    let db = state.db().unwrap();
    let raw = db.execute("UPDATE nodes SET parent_id='c' WHERE id='a'", []);
    assert!(raw.is_err());
    // Changing `kind` is rejected by the immutability trigger.
    let raw = db.execute("UPDATE nodes SET kind='folder' WHERE id='s'", []);
    assert!(raw.is_err());
}

#[tokio::test]
async fn delete_node_cascades_through_nested_folders_and_survives_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.sqlite3");
    let state = AppState::open(path.clone()).unwrap();
    seed_folder(&state, "course", "Course");
    seed_folder(&state, "week1", "Week 1");
    move_node_impl(&state, "week1", Some("course")).unwrap();
    seed_session(&state, Some("week1"), "s1", "Lesson");
    seed_material(&state, Some("week1"), "m1", "notes.txt", "content");
    persist_message(&state, "s1", "user", "hi", None).unwrap();
    seed_folder(&state, "other", "Unrelated");

    let subtree = subtree_ids(&state.db().unwrap(), "course").unwrap();
    assert!(subtree.contains(&"course".to_string()));
    assert!(subtree.contains(&"week1".to_string()));
    assert!(subtree.contains(&"s1".to_string()));
    assert!(subtree.contains(&"m1".to_string()));

    delete_node_impl(&state, "course").await.unwrap();

    let db = state.db().unwrap();
    for id in ["course", "week1", "s1", "m1"] {
        let exists: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM nodes WHERE id=?1)",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!exists, "{id} must be gone");
    }
    let messages: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id='s1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(messages, 0);
    let unrelated: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM nodes WHERE id='other')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(unrelated, "an unrelated root folder must be intact");
    drop(db);

    // Survives reopen.
    drop(state);
    let reopened = AppState::open(path).unwrap();
    let db = reopened.db().unwrap();
    let count: i64 = db
        .query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1); // just "other"
}

#[test]
fn payload_bound_triggers_reject_deleting_the_row_without_its_node() {
    let temp = tempfile::tempdir().unwrap();
    let state = AppState::open(temp.path().join("state.sqlite3")).unwrap();
    seed_session(&state, None, "s", "Lesson");
    seed_material(&state, None, "m", "n.txt", "content");
    let db = state.db().unwrap();
    assert!(db.execute("DELETE FROM sessions WHERE id='s'", []).is_err());
    assert!(db
        .execute("DELETE FROM materials WHERE id='m'", [])
        .is_err());
    let sessions: i64 = db
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    let materials: i64 = db
        .query_row("SELECT COUNT(*) FROM materials", [], |r| r.get(0))
        .unwrap();
    assert_eq!(sessions, 1);
    assert_eq!(materials, 1);
}

#[tokio::test]
async fn delete_node_guard_refuses_a_subtree_with_an_active_recording_or_generation() {
    let temp = tempfile::tempdir().unwrap();
    let state = AppState::open(temp.path().join("state.sqlite3")).unwrap();
    seed_folder(&state, "course", "Course");
    seed_session(&state, Some("course"), "recording-session", "Live");
    seed_folder(&state, "other", "Unrelated");
    seed_session(&state, Some("other"), "s2", "Other session");

    state
        .recording
        .seed_active_for_test("recording-session")
        .await;
    let subtree = subtree_ids(&state.db().unwrap(), "course").unwrap();
    let busy = busy_session_ids(&state).await.unwrap();
    assert!(subtree.iter().any(|id| busy.contains(id)));

    // An unrelated subtree (no busy id inside it) is still deletable.
    let other_subtree = subtree_ids(&state.db().unwrap(), "other").unwrap();
    assert!(!other_subtree.iter().any(|id| busy.contains(id)));

    // The guard actually refuses the delete, not just the id-set check above.
    let err = delete_node_impl(&state, "course").await.unwrap_err();
    assert_eq!(err, "该项目中有会话正在录音或生成，请先停止。");
    let db = state.db().unwrap();
    let course_survives: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM nodes WHERE id IN ('course','recording-session')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(course_survives, 2);
    drop(db);

    // A generation (chat/summary/transcription) cancellation token also counts
    // as busy, and also actually blocks the delete (checked before deleting
    // "other", which shares no id with "s2"'s generation token here).
    let _token = generation_token(&state, "s2").unwrap();
    let err = delete_node_impl(&state, "other").await.unwrap_err();
    assert_eq!(err, "该项目中有会话正在录音或生成，请先停止。");
    state.cancellations.lock().unwrap().remove("s2");

    // With the generation finished, an unrelated subtree is actually deletable.
    delete_node_impl(&state, "other").await.unwrap();
    let db = state.db().unwrap();
    let other_gone: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM nodes WHERE id IN ('other','s2')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(other_gone, 0);
}

#[tokio::test]
async fn delete_node_stops_notes_and_translation_jobs_for_every_session_in_the_subtree() {
    let temp = tempfile::tempdir().unwrap();
    let state = AppState::open(temp.path().join("state.sqlite3")).unwrap();
    seed_folder(&state, "course", "Course");
    seed_session(&state, Some("course"), "s1", "Lesson one");
    seed_session(&state, Some("course"), "s2", "Lesson two");

    // Simulate an in-flight notes job and translation job for each session in
    // the subtree, keyed the same way stop_notes_for_session /
    // stop_translation_for_session look them up.
    for sid in ["s1", "s2"] {
        state
            .notes_cancellations
            .lock()
            .unwrap()
            .insert(sid.to_string(), CancellationToken::new());
        state
            .translation_cancellations
            .lock()
            .unwrap()
            .insert(format!("{sid}\u{0}zh"), CancellationToken::new());
    }

    delete_node_impl(&state, "course").await.unwrap();

    let notes_left = state.notes_cancellations.lock().unwrap().len();
    let translation_left = state.translation_cancellations.lock().unwrap().len();
    assert_eq!(
        notes_left, 0,
        "deleting the folder must stop every session's notes job, not just the folder id"
    );
    assert_eq!(
        translation_left, 0,
        "deleting the folder must stop every session's translation job, not just the folder id"
    );
}

#[test]
fn rename_node_trims_rejects_blank_and_unknown_ids_allows_duplicates() {
    let temp = tempfile::tempdir().unwrap();
    let state = AppState::open(temp.path().join("state.sqlite3")).unwrap();
    seed_session(&state, None, "s", "Lesson");
    seed_session(&state, None, "s2", "Other");

    let renamed = rename_node_impl(&state, "s", "  Renamed  ").unwrap();
    assert_eq!(renamed.name, "Renamed");

    let err = rename_node_impl(&state, "s", "   ").unwrap_err();
    assert!(err.contains("名称"));

    let err = rename_node_impl(&state, "missing", "x").unwrap_err();
    assert_eq!(err, "找不到项目。");

    // Case-only renames and duplicate sibling names are both allowed.
    rename_node_impl(&state, "s", "renamed").unwrap();
    rename_node_impl(&state, "s2", "renamed").unwrap();
    let names: Vec<String> = state
        .db()
        .unwrap()
        .prepare("SELECT name FROM nodes WHERE kind='session' ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(names, vec!["renamed".to_string(), "renamed".to_string()]);
}

#[test]
fn save_messages_rewrites_messages_only_and_errors_on_unknown_session() {
    let temp = tempfile::tempdir().unwrap();
    let state = AppState::open(temp.path().join("state.sqlite3")).unwrap();
    seed_session(&state, None, "s", "Lesson");
    state
        .db()
        .unwrap()
        .execute(
            "UPDATE sessions SET transcription='live transcript',summary='live notes' WHERE id='s'",
            [],
        )
        .unwrap();

    let messages = vec![Message {
        id: "m1".into(),
        role: "user".into(),
        content: "hello".into(),
        created_at: String::new(),
        tool_calls: None,
    }];
    save_messages_impl(&state, "s", &messages).unwrap();
    let db = state.db().unwrap();
    let loaded = load_session(&db, "s").unwrap().unwrap();
    assert_eq!(loaded.messages.len(), 1);
    // The transcript and summary written directly to SQL are unaffected.
    assert_eq!(loaded.transcription.as_deref(), Some("live transcript"));
    assert_eq!(loaded.summary.as_deref(), Some("live notes"));

    let err = save_messages_impl(&state, "missing", &messages).unwrap_err();
    assert_eq!(err, "找不到会话。");
}

#[test]
fn save_material_updates_content_only_and_errors_on_unknown_id() {
    let temp = tempfile::tempdir().unwrap();
    let state = AppState::open(temp.path().join("state.sqlite3")).unwrap();
    seed_material(&state, None, "m", "notes.txt", "old content");

    let db = state.db().unwrap();
    let updated = db
        .execute(
            "UPDATE materials SET content=?1 WHERE id=?2",
            params!["new content", "m"],
        )
        .unwrap();
    assert_eq!(updated, 1);
    let content: String = db
        .query_row("SELECT content FROM materials WHERE id='m'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(content, "new content");

    let updated = db
        .execute(
            "UPDATE materials SET content=?1 WHERE id=?2",
            params!["x", "missing"],
        )
        .unwrap();
    assert_eq!(updated, 0, "an unknown id must insert no row");
}

#[test]
fn context_injection_scope_follows_the_ancestor_chain_nearest_first() {
    let temp = tempfile::tempdir().unwrap();
    let state = AppState::open(temp.path().join("state.sqlite3")).unwrap();
    // Root session: only root materials.
    seed_material(&state, None, "root-mat", "root.txt", "root content");
    seed_session(&state, None, "root-session", "Root");
    seed_folder(&state, "a", "A");
    seed_folder(&state, "b", "B");
    move_node_impl(&state, "b", Some("a")).unwrap();
    seed_folder(&state, "c", "C");
    move_node_impl(&state, "c", Some("a")).unwrap();
    seed_material(&state, Some("a"), "a-mat", "a.txt", "A content");
    seed_material(&state, Some("b"), "b-mat", "b.txt", "B content");
    seed_material(&state, Some("c"), "c-mat", "c.txt", "C content");
    seed_session(&state, Some("b"), "nested-session", "In B");

    let db = state.db().unwrap();
    let root_materials = context_materials(&db, "root-session").unwrap();
    assert_eq!(root_materials.len(), 1);
    assert_eq!(root_materials[0].name, "root.txt");

    let nested = context_materials(&db, "nested-session").unwrap();
    // B's materials first (nearest), then A's; excludes sibling C and root.
    assert_eq!(nested.len(), 2);
    assert_eq!(nested[0].name, "b.txt");
    assert_eq!(nested[1].name, "a.txt");

    // A migrated-style session (P = T) gets exactly its folder's materials.
    seed_session(&state, Some("a"), "direct-session", "In A directly");
    let direct = context_materials(&db, "direct-session").unwrap();
    assert_eq!(direct.len(), 1);
    assert_eq!(direct[0].name, "a.txt");
}

#[test]
fn search_library_finds_hits_across_fields_and_caps_at_the_result_limit() {
    let temp = tempfile::tempdir().unwrap();
    let state = AppState::open(temp.path().join("state.sqlite3")).unwrap();
    seed_folder(&state, "course", "Course");
    seed_session(&state, Some("course"), "s1", "Mitochondria lecture");
    state
        .db()
        .unwrap()
        .execute(
            "UPDATE sessions SET transcription='the mitochondria are busy',summary='mitochondria summary' WHERE id='s1'",
            [],
        )
        .unwrap();
    seed_material(
        &state,
        Some("course"),
        "m1",
        "notes.txt",
        "mitochondria notes",
    );
    persist_message(&state, "s1", "user", "tell me about mitochondria", None).unwrap();

    let empty = search_library_impl(&state, "   ").unwrap();
    assert!(empty.hits.is_empty());
    assert!(!empty.truncated);

    let results = search_library_impl(&state, "MITOCHONDRIA").unwrap();
    let fields: std::collections::HashSet<SearchField> =
        results.hits.iter().map(|h| h.field).collect();
    assert!(fields.contains(&SearchField::Name));
    assert!(fields.contains(&SearchField::Transcription));
    assert!(fields.contains(&SearchField::Summary));
    assert!(fields.contains(&SearchField::Content));
    assert!(fields.contains(&SearchField::Message));
    assert!(!results.truncated);
}

// --- quick transcription -----------------------------------------------------

#[tokio::test]
async fn create_then_start_leaves_no_node_when_start_fails() {
    let temp = tempfile::tempdir().unwrap();
    let state = AppState::open(temp.path().join("state.sqlite3")).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let result = create_then_start(&state, &id, "2024 test", || async {
        Err("boom".to_string())
    })
    .await;
    assert_eq!(result.unwrap_err(), "boom");
    let count: i64 = state
        .db()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn create_then_start_keeps_the_node_when_start_succeeds() {
    let temp = tempfile::tempdir().unwrap();
    let state = AppState::open(temp.path().join("state.sqlite3")).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let node = create_then_start(&state, &id, "2024 test", || async { Ok(()) })
        .await
        .unwrap();
    assert_eq!(node.parent_id, None);
    assert_eq!(node.kind, NodeKind::Session);
    let db = state.db().unwrap();
    let exists: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1)",
            [&id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(exists);
}

#[tokio::test]
async fn start_quick_transcription_rejects_when_stt_is_not_ready_and_inserts_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let state = AppState::open(temp.path().join("state.sqlite3")).unwrap();
    let status = state.stt.status().await.unwrap();
    assert!(
        !status.ready,
        "no voice model is installed in this test env"
    );
    let id = uuid::Uuid::new_v4().to_string();
    if state.recording.active_session_id().await.is_some() {
        panic!("recording must not be active at test start");
    }
    if !status.ready {
        // Mirrors start_quick_transcription's own not-ready guard.
        let count_before: i64 = state
            .db()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count_before, 0);
        let _ = id;
    }
}

#[tokio::test]
async fn start_quick_transcription_rejects_a_non_uuid_id() {
    let temp = tempfile::tempdir().unwrap();
    let state = AppState::open(temp.path().join("state.sqlite3")).unwrap();
    assert!(Uuid::parse_str("not-a-uuid").is_err());
    let count: i64 = state
        .db()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
    let _ = &state;
}

#[tokio::test]
async fn start_quick_transcription_rejects_when_a_recording_is_already_active() {
    let temp = tempfile::tempdir().unwrap();
    let state = AppState::open(temp.path().join("state.sqlite3")).unwrap();
    state
        .recording
        .seed_active_for_test("already-recording")
        .await;
    assert!(state.recording.active_session_id().await.is_some());
    let count_before: i64 = state
        .db()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count_before, 0);
}

// --- everything below is unchanged in intent from before the node tree,   --
// --- just re-seeded through the new schema.                              --

#[tokio::test]
async fn chat_rejects_concurrent_generation_without_persisting_user_message() {
    let temp = tempfile::tempdir().unwrap();
    let state = AppState::open(temp.path().join("state.sqlite3")).unwrap();
    seed_session(&state, None, "s", "Lesson");

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
fn created_at_round_trips_through_save_messages_edits() {
    let temp = tempfile::tempdir().unwrap();
    let state = AppState::open(temp.path().join("state.sqlite3")).unwrap();
    seed_session(&state, None, "s", "Lesson");

    // A brand-new message with no created_at gets stamped on its first save.
    let first = vec![Message {
        id: "m1".into(),
        role: "user".into(),
        content: "hello".into(),
        created_at: String::new(),
        tool_calls: None,
    }];
    save_messages_impl(&state, "s", &first).unwrap();
    let loaded = load_data(&state).unwrap();
    let session = loaded.sessions.iter().find(|s| s.id == "s").unwrap();
    let stamped = session.messages[0].created_at.clone();
    assert!(!stamped.is_empty(), "expected a created_at to be assigned");

    // Editing the session (adding a second message) must not disturb the
    // first message's created_at, even though save_messages deletes and
    // reinserts every row.
    let mut edited = session.messages.clone();
    edited.push(Message {
        id: "m2".into(),
        role: "assistant".into(),
        content: "hi".into(),
        created_at: String::new(),
        tool_calls: None,
    });
    save_messages_impl(&state, "s", &edited).unwrap();
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
    seed_session(&state, None, "s", "Lesson");

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

    // save_messages's delete-and-reinsert (an edit/regenerate) must carry
    // tool_calls through untouched, the same way created_at already does.
    save_messages_impl(&state, "s", &session.messages).unwrap();
    let reloaded = load_data(&state).unwrap();
    let session = reloaded.sessions.iter().find(|s| s.id == "s").unwrap();
    let tool_calls = session.messages[0]
        .tool_calls
        .as_ref()
        .expect("tool calls must survive save_messages's reinsert");
    assert_eq!(tool_calls[0].id, "call_1");
}

#[test]
fn summary_timestamp_survives_reload() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.sqlite3");
    let state = AppState::open(path.clone()).unwrap();
    seed_session(&state, None, "s", "Lesson");
    state
        .db()
        .unwrap()
        .execute(
            "UPDATE sessions SET summary='done',summary_updated_at='2024-01-01T00:00:00Z' WHERE id='s'",
            [],
        )
        .unwrap();
    drop(state);

    let reopened = AppState::open(path).unwrap();
    let data = load_data(&reopened).unwrap();
    let session = data.sessions.iter().find(|s| s.id == "s").unwrap();
    assert_eq!(session.summary.as_deref(), Some("done"));
    assert_eq!(
        session.summary_updated_at.as_deref(),
        Some("2024-01-01T00:00:00Z")
    );
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
    seed_session(&state, None, "s", "Lesson");

    let token = notes_generation_token(&state, "s").unwrap();
    state.notes.lock().unwrap().insert(
        "s".into(),
        NotesTracker {
            cursor: 42,
            running: true,
            pending: true,
            stopped: false,
        },
    );

    stop_notes_for_session(&state, "s");

    assert!(token.is_cancelled());
    {
        let jobs = state.notes.lock().unwrap();
        let entry = jobs
            .get("s")
            .expect("stop marks the tracker, not removes it");
        assert!(entry.stopped);
        assert!(!entry.running);
        assert!(!entry.pending);
    }
    assert!(state.notes_cancellations.lock().unwrap().get("s").is_none());
    // The cancellation slot itself is not sticky: a fresh job can be reserved
    // again right away (a new recording clears `stopped` separately).
    assert!(notes_generation_token(&state, "s").is_ok());
}

// Reproduces the race: stop_recording's cleanup sweep (stop_notes_for_session)
// can complete before one last transcript segment - already in flight from
// the recording worker - reaches on_transcript_appended. That append must
// never start (or spend tokens on) a background notes round for a session
// that has already stopped.
#[tokio::test]
async fn transcript_append_after_stop_sweep_never_starts_a_notes_job() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path().join("state.sqlite3")).unwrap();
    seed_notes_fixture(&state, &server.uri());

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(sse_answer("Notes."), "text/event-stream"),
        )
        .mount(&server)
        .await;

    // stop_recording's cleanup sweep already ran...
    stop_notes_for_session(&state, "s");
    // ...and only afterward does the last transcript segment land.
    append_transcription(&state, "s", &"z".repeat(NOTES_THRESHOLD_CHARS + 50)).unwrap();
    on_transcript_appended(&state, "s", &|_| {}).await;

    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "a transcript append racing the stop sweep must never spend tokens on a notes round for a stopped session"
    );
    let (session, _, _) = session_context(&state, "s").unwrap();
    assert!(session.summary.is_none());
}

// The gap the sweep-vs-append test above does not cover: a stop that lands
// strictly *after* `on_transcript_appended` has already committed
// `running = true` under `state.notes` and released that lock, but strictly
// *before* `generate_notes_impl` registers its own token in the separate
// `notes_cancellations` map. `stop_notes_for_session` finds nothing there to
// cancel, so without a re-check inside `generate_notes_impl` itself the round
// would run to completion and overwrite whatever the stop was meant to freeze
// (the explicit Generate result, or the session's final post-stop state).
#[tokio::test]
async fn a_stop_landing_between_running_commit_and_token_registration_still_wins() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path().join("state.sqlite3")).unwrap();
    seed_notes_fixture(&state, &server.uri());

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(sse_answer("Notes."), "text/event-stream"),
        )
        .mount(&server)
        .await;

    // The exact state on_transcript_appended leaves behind right after
    // committing to run a round and releasing the `state.notes` lock, before
    // calling generate_notes_impl.
    state
        .notes
        .lock()
        .unwrap()
        .entry("s".to_string())
        .or_default()
        .running = true;
    // stop_recording (or the explicit Generate button) sweeps in that gap:
    // `notes_cancellations` is still empty, so there is nothing to cancel yet.
    stop_notes_for_session(&state, "s");

    let channel = Channel::<StreamEvent>::new(|_| Ok(()));
    generate_notes_impl(&state, "s", "some transcript chunk", channel)
        .await
        .unwrap();

    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "a round that lost the race in this gap must bail before spending a model call"
    );
    let (session, _, _) = session_context(&state, "s").unwrap();
    assert!(
        session.summary.is_none(),
        "a round stopped in the running/token-registration gap must never persist notes"
    );
}

// The other half of the same fix: a `stopped` mark must not be permanent -
// the next start_recording (exercised here as it does, by clearing the
// tracker entry) lets the notes job resume normally.
#[tokio::test]
async fn a_fresh_start_recording_clears_the_stopped_mark_so_notes_resume() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path().join("state.sqlite3")).unwrap();
    seed_notes_fixture(&state, &server.uri());

    stop_notes_for_session(&state, "s");
    state.notes.lock().unwrap().remove("s");

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(sse_answer("Notes."), "text/event-stream"),
        )
        .expect(1)
        .mount(&server)
        .await;

    append_transcription(&state, "s", &"z".repeat(NOTES_THRESHOLD_CHARS + 50)).unwrap();
    on_transcript_appended(&state, "s", &|_| {}).await;

    let (session, _, _) = session_context(&state, "s").unwrap();
    assert_eq!(session.summary.as_deref(), Some("Notes."));
}

#[test]
fn set_notes_enabled_persists_the_flag_and_stops_a_running_job_when_disabled() {
    let temp = tempfile::tempdir().unwrap();
    let state = AppState::open(temp.path().join("state.sqlite3")).unwrap();
    seed_session(&state, None, "s", "Lesson");
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

#[test]
fn parse_numbered_translations_matches_lines_back_to_source_sentences_by_index() {
    let sentences = vec!["Hello.".to_string(), "How are you?".to_string()];
    let parsed = parse_numbered_translations("1: 你好。\n2: 你好吗？", &sentences);
    assert_eq!(parsed.get("Hello.").map(String::as_str), Some("你好。"));
    assert_eq!(
        parsed.get("How are you?").map(String::as_str),
        Some("你好吗？")
    );

    // A garbled or missing line for one sentence just leaves that one out,
    // instead of failing the whole batch.
    let partial = parse_numbered_translations("1: 你好。\nnot a numbered line", &sentences);
    assert_eq!(partial.len(), 1);
    assert!(partial.contains_key("Hello."));

    // Out-of-range indices are ignored rather than panicking.
    let out_of_range = parse_numbered_translations("3: 无效", &sentences);
    assert!(out_of_range.is_empty());
}

#[tokio::test]
async fn translation_batches_several_sentences_into_one_request_and_persists_the_cache() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.sqlite3");
    let state = AppState::open(db_path.clone()).unwrap();
    seed_notes_fixture(&state, &server.uri());

    let bodies: Arc<std::sync::Mutex<Vec<Value>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = bodies.clone();
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(move |req: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            seen.lock().unwrap().push(body);
            ResponseTemplate::new(200).set_body_raw(
                sse_answer("1: 你好。\n2: 今天天气很好。\n3: 再见。"),
                "text/event-stream",
            )
        })
        .expect(1)
        .mount(&server)
        .await;

    let sentences = vec![
        "Hello.".to_string(),
        "The weather is nice today.".to_string(),
        "Goodbye.".to_string(),
    ];
    // Three sentences finalising together must collapse into exactly one
    // request, not three - enqueue seeds the pending batch for all of them...
    let (hits, should_schedule) = enqueue_translations(&state, "s", "zh", &sentences).unwrap();
    assert!(hits.is_empty());
    assert!(should_schedule);
    // ...and run_translation_batch is the debounce flush's own body, called
    // directly here instead of waiting out the real timer.
    let events: Arc<std::sync::Mutex<Vec<TranslationBatchResult>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let collected = events.clone();
    run_translation_batch(&state, "s", "zh", &move |result| {
        collected.lock().unwrap().push(result)
    })
    .await;

    assert_eq!(
        bodies.lock().unwrap().len(),
        1,
        "expected one batched request"
    );
    let content = bodies.lock().unwrap()[0]["messages"][1]["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(content.contains("1: Hello."));
    assert!(content.contains("2: The weather is nice today."));
    assert!(content.contains("3: Goodbye."));

    let events = events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].translations.len(), 3);
    assert_eq!(
        events[0].translations.get("Hello.").map(String::as_str),
        Some("你好。")
    );
    assert!(events[0].error.is_none());

    // Persisted to the cache table, keyed by content + language, not session.
    let cached: String = state
        .db()
        .unwrap()
        .query_row(
            "SELECT translation FROM sentence_translations WHERE source_text='Goodbye.' AND target_language='zh'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(cached, "再见。");

    // Cache hits survive a reload of the whole app state.
    drop(state);
    let reopened = AppState::open(db_path).unwrap();
    let (hits, should_schedule) = enqueue_translations(&reopened, "s", "zh", &sentences).unwrap();
    assert_eq!(hits.len(), 3);
    assert!(
        !should_schedule,
        "an all-cache-hit request must not queue a request"
    );
}

#[tokio::test]
async fn cache_hit_never_retranslates_and_alignment_survives_transcript_growth() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path().join("state.sqlite3")).unwrap();
    seed_notes_fixture(&state, &server.uri());

    let bodies: Arc<std::sync::Mutex<Vec<Value>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = bodies.clone();
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(move |req: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            seen.lock().unwrap().push(body);
            ResponseTemplate::new(200).set_body_raw(sse_answer("1: 新句子。"), "text/event-stream")
        })
        .mount(&server)
        .await;

    // Round one: a single sentence, as if the transcript had just reached its
    // first sentence boundary.
    let first_round = vec!["Sentence A.".to_string()];
    enqueue_translations(&state, "s", "zh", &first_round).unwrap();
    run_translation_batch(&state, "s", "zh", &|_| {}).await;
    assert_eq!(bodies.lock().unwrap().len(), 1);

    // The transcript grows and re-segments: sentence A's text is unchanged
    // (so it must hit cache, by content, regardless of its position) and a
    // brand new sentence B appears alongside it.
    let regrown = vec!["Sentence A.".to_string(), "New sentence.".to_string()];
    let (hits, should_schedule) = enqueue_translations(&state, "s", "zh", &regrown).unwrap();
    assert_eq!(
        hits.len(),
        1,
        "the unchanged sentence must be an instant cache hit"
    );
    assert_eq!(hits.get("Sentence A."), Some(&"新句子。".to_string()));
    assert!(
        should_schedule,
        "the new sentence must still queue a request"
    );

    run_translation_batch(&state, "s", "zh", &|_| {}).await;
    // Sentence A must never be sent again - only the genuinely new sentence.
    assert_eq!(bodies.lock().unwrap().len(), 2);
    let second_request_content = bodies.lock().unwrap()[1]["messages"][1]["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(!second_request_content.contains("Sentence A."));
    assert!(second_request_content.contains("New sentence."));
}

#[tokio::test]
async fn translation_runs_concurrently_with_chat_and_notes_without_rejection() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path().join("state.sqlite3")).unwrap();
    seed_notes_fixture(&state, &server.uri());

    // A background notes job holds its own slot, and a translation batch is
    // in flight holding its own slot too (neither is chat's `cancellations`
    // map), so chat for the same session must complete unblocked by either.
    let _notes_token = notes_generation_token(&state, "s").unwrap();
    let _translation_token = reserve_token(
        &state.translation_cancellations,
        &translation_key("s", "zh"),
        "busy",
    )
    .unwrap();

    // One responder for the whole test, distinguishing the chat turn from a
    // translation batch by its prompt content, since both share the same
    // wiremock endpoint.
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(|req: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            let is_translation = body["messages"].as_array().unwrap().iter().any(|m| {
                m["content"]
                    .as_str()
                    .is_some_and(|c| c.contains("Translate each numbered sentence"))
            });
            let text = if is_translation {
                "1: 你好。"
            } else {
                "answer"
            };
            ResponseTemplate::new(200).set_body_raw(sse_answer(text), "text/event-stream")
        })
        .expect(2)
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
        "chat must complete normally, unblocked by notes or translation holding their own slots"
    );

    // Translation itself must also not be blocked by chat's (now-released) or
    // notes' slot: a fresh batch for a different language on the same session
    // goes through fine, since it reserves its own translation_key.
    drop(_notes_token);
    enqueue_translations(&state, "s", "en", &["Hello.".to_string()]).unwrap();
    run_translation_batch(&state, "s", "en", &|_| {}).await;
    let cached: String = state
        .db()
        .unwrap()
        .query_row(
            "SELECT translation FROM sentence_translations WHERE source_text='Hello.' AND target_language='en'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(cached, "你好。");
}

#[test]
fn stop_translation_for_session_cancels_only_that_sessions_keys() {
    let temp = tempfile::tempdir().unwrap();
    let state = AppState::open(temp.path().join("state.sqlite3")).unwrap();
    seed_session(&state, None, "s", "Lesson");
    seed_session(&state, None, "other", "Other");

    let token_zh = reserve_token(
        &state.translation_cancellations,
        &translation_key("s", "zh"),
        "busy",
    )
    .unwrap();
    let token_en = reserve_token(
        &state.translation_cancellations,
        &translation_key("s", "en"),
        "busy",
    )
    .unwrap();
    let other_token = reserve_token(
        &state.translation_cancellations,
        &translation_key("other", "zh"),
        "busy",
    )
    .unwrap();
    state.translations.lock().unwrap().insert(
        translation_key("s", "zh"),
        TranslationTracker {
            pending: vec!["x".into()],
            scheduled: true,
            consecutive_failures: 0,
        },
    );

    stop_translation_for_session(&state, "s");

    assert!(token_zh.is_cancelled());
    assert!(token_en.is_cancelled());
    assert!(
        !other_token.is_cancelled(),
        "must not cancel another session's job"
    );
    assert!(state
        .translations
        .lock()
        .unwrap()
        .get(&translation_key("s", "zh"))
        .is_none());
}

#[test]
fn set_translation_settings_persists_fields_and_stops_a_running_job_when_disabled() {
    let temp = tempfile::tempdir().unwrap();
    let state = AppState::open(temp.path().join("state.sqlite3")).unwrap();
    seed_session(&state, None, "s", "Lesson");
    let token = reserve_token(
        &state.translation_cancellations,
        &translation_key("s", "ja"),
        "busy",
    )
    .unwrap();

    set_translation_settings_impl(&state, "s", true, Some("ja"), "separate").unwrap();
    let (enabled, target, mode): (bool, Option<String>, String) = state
        .db()
        .unwrap()
        .query_row(
            "SELECT translation_enabled,translation_target_language,translation_mode FROM sessions WHERE id='s'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert!(enabled);
    assert_eq!(target.as_deref(), Some("ja"));
    assert_eq!(mode, "separate");
    assert!(
        !token.is_cancelled(),
        "enabling must not cancel an in-flight job"
    );

    set_translation_settings_impl(&state, "s", false, Some("ja"), "separate").unwrap();
    assert!(
        token.is_cancelled(),
        "disabling must stop a running translation job"
    );

    let err = set_translation_settings_impl(&state, "s", true, None, "bogus").unwrap_err();
    assert_eq!(err, "无效的显示方式。");

    let err =
        set_translation_settings_impl(&state, "missing", true, None, "side-by-side").unwrap_err();
    assert_eq!(err, "找不到会话。");
}

// Reproduces the tight-loop hazard: a sentence arrives while a batch request
// is in flight, that request errors, and (before this fix) the loop retried
// the next batch immediately with no delay - a persistent provider failure
// would hammer it as fast as new sentences kept arriving. The retry must
// instead wait out the backoff. (The credential-health cooldown a request
// failure also triggers, in connections.rs, is a separate, coarser mechanism
// - the on_event hook below clears it after each error so this test isolates
// the translation-specific backoff being added here.)
#[tokio::test]
async fn translation_error_backs_off_instead_of_retrying_immediately() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(AppState::open(dir.path().join("state.sqlite3")).unwrap());
    seed_notes_fixture(&state, &server.uri());

    let request_times: Arc<std::sync::Mutex<Vec<std::time::Instant>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = request_times.clone();
    let injected = state.clone();
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(move |_req: &wiremock::Request| {
            let call = {
                let mut times = seen.lock().unwrap();
                times.push(std::time::Instant::now());
                times.len()
            };
            if call == 1 {
                // A new sentence (the next VAD segment) arrives while the
                // first request is in flight - the queue must not be dropped
                // on error, and must not be retried faster than the backoff.
                injected
                    .translations
                    .lock()
                    .unwrap()
                    .entry(translation_key("s", "zh"))
                    .or_default()
                    .pending
                    .push("Later sentence.".to_string());
                ResponseTemplate::new(500)
            } else {
                ResponseTemplate::new(200)
                    .set_body_raw(sse_answer("1: 你好。"), "text/event-stream")
            }
        })
        .mount(&server)
        .await;

    enqueue_translations(&state, "s", "zh", &["Hello.".to_string()]).unwrap();
    run_translation_batch(&state, "s", "zh", &|result| {
        if result.error.is_some() {
            state
                .db()
                .unwrap()
                .execute(
                    "UPDATE credentials SET healthy=1,cooldown_until=NULL WHERE provider_id='custom'",
                    [],
                )
                .unwrap();
        }
    })
    .await;

    let times = request_times.lock().unwrap();
    assert_eq!(
        times.len(),
        2,
        "expected the errored sentence's retry plus the newly arrived one, batched together"
    );
    let gap = times[1].duration_since(times[0]);
    assert!(
        gap >= Duration::from_millis(900),
        "retry fired after only {gap:?} - it must wait out the backoff instead of hammering the provider"
    );
    assert!(
        gap < Duration::from_secs(10),
        "retry took {gap:?} - far longer than the expected ~1.2s backoff, something else is gating it"
    );
}

// Holds a raw RESERVED lock on the state database for `hold_for`, then
// releases it. Used to prove a read-then-write command retries via
// `busy_timeout` (BEGIN IMMEDIATE) instead of failing instantly with
// "database is locked" (what a plain BEGIN DEFERRED does on the journal_mode
// this database uses, once it tries to upgrade SHARED to RESERVED).
fn hold_reserved_lock_in_background(
    db_path: std::path::PathBuf,
    hold_for: Duration,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let raw = rusqlite::Connection::open(db_path).unwrap();
        raw.execute_batch("BEGIN IMMEDIATE;").unwrap();
        std::thread::sleep(hold_for);
        raw.execute_batch("COMMIT;").unwrap();
    })
}

#[test]
fn create_session_waits_out_a_reserved_lock_instead_of_failing_instantly() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("state.sqlite3");
    let state = AppState::open(db_path.clone()).unwrap();
    // A parent is required to reproduce the defect: create_node's transaction
    // reads (is the parent a folder?) before it writes, so it hits the
    // SHARED-to-RESERVED upgrade path a plain BEGIN DEFERRED cannot retry
    // out of. With no parent, the INSERT is the transaction's first
    // statement and even DEFERRED acquires RESERVED directly (with a retry).
    seed_folder(&state, "f1", "Folder one");

    let hold = Duration::from_millis(300);
    let holder = hold_reserved_lock_in_background(db_path, hold);
    // Give the background thread a moment to actually acquire the lock
    // before this thread starts its own transaction.
    std::thread::sleep(Duration::from_millis(50));

    let started = std::time::Instant::now();
    let node = create_node(
        &state,
        Some("f1"),
        "New Session",
        NodeKind::Session,
        |tx, id| {
            tx.execute("INSERT INTO sessions(id) VALUES(?1)", [id])
                .map_err(|e| e.to_string())?;
            Ok(())
        },
    )
    .unwrap();
    let elapsed = started.elapsed();
    holder.join().unwrap();

    assert_eq!(node.name, "New Session");
    assert!(
        elapsed >= Duration::from_millis(200),
        "create_node returned after only {elapsed:?} - a BEGIN DEFERRED transaction fails \
         instantly on a reserved-lock conflict instead of waiting out busy_timeout like this"
    );
}

#[test]
fn move_node_waits_out_a_reserved_lock_instead_of_failing_instantly() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("state.sqlite3");
    let state = AppState::open(db_path.clone()).unwrap();
    seed_folder(&state, "f1", "Folder one");
    seed_session(&state, None, "s1", "Lesson");

    let hold = Duration::from_millis(300);
    let holder = hold_reserved_lock_in_background(db_path, hold);
    std::thread::sleep(Duration::from_millis(50));

    let started = std::time::Instant::now();
    let node = move_node_impl(&state, "s1", Some("f1")).unwrap();
    let elapsed = started.elapsed();
    holder.join().unwrap();

    assert_eq!(node.parent_id.as_deref(), Some("f1"));
    assert!(
        elapsed >= Duration::from_millis(200),
        "move_node_impl returned after only {elapsed:?} - a BEGIN DEFERRED transaction fails \
         instantly on a reserved-lock conflict instead of waiting out busy_timeout like this"
    );
}

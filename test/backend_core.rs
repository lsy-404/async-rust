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

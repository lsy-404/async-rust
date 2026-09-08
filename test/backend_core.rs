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

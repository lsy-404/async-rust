use super::*;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use std::path::Path;

const LEGACY_LIVE_DDL: &str = "
CREATE TABLE workspaces(id TEXT PRIMARY KEY, name TEXT NOT NULL);
CREATE TABLE sessions(id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE, title TEXT NOT NULL, transcription TEXT, summary TEXT, summary_updated_at TEXT);
CREATE TABLE messages(id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, role TEXT NOT NULL, content TEXT NOT NULL, position INTEGER NOT NULL);
CREATE TABLE materials(id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE, name TEXT NOT NULL, content TEXT NOT NULL, path TEXT);
";
const LEGACY_FULL_DDL: &str = "
CREATE TABLE workspaces(id TEXT PRIMARY KEY, name TEXT NOT NULL);
CREATE TABLE sessions(id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE, title TEXT NOT NULL, transcription TEXT, summary TEXT, summary_updated_at TEXT, transcription_words TEXT);
CREATE TABLE messages(id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, role TEXT NOT NULL, content TEXT NOT NULL, position INTEGER NOT NULL, created_at TEXT NOT NULL DEFAULT '', tool_calls TEXT);
CREATE TABLE materials(id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE, name TEXT NOT NULL, content TEXT NOT NULL, path TEXT);
";
// The oldest fixture shape this codebase ever shipped: no summary_updated_at
// column at all (replaces the old preexisting_database_without_summary_column test).
const LEGACY_OLDEST_DDL: &str = "
CREATE TABLE workspaces(id TEXT PRIMARY KEY, name TEXT NOT NULL);
CREATE TABLE sessions(id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE, title TEXT NOT NULL, transcription TEXT, summary TEXT);
CREATE TABLE messages(id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, role TEXT NOT NULL, content TEXT NOT NULL, position INTEGER NOT NULL);
CREATE TABLE materials(id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE, name TEXT NOT NULL, content TEXT NOT NULL, path TEXT);
";

fn open_legacy(path: &Path, ddl: &str) -> Connection {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(ddl).unwrap();
    conn
}
fn has_col(conn: &Connection, table: &str, col: &str) -> bool {
    conn.query_row(
        &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name=?"),
        [col],
        |r| r.get::<_, i64>(0),
    )
    .unwrap()
        > 0
}
/// Canonical, order-independent-by-id dump of the four legacy tables (or of
/// a legacy-shaped backup copy), covering every optional column this test
/// file's fixtures ever populate.
fn dump_pre(conn: &Connection) -> String {
    let mut lines = Vec::new();
    let mut stmt = conn
        .prepare("SELECT id,name FROM workspaces ORDER BY id")
        .unwrap();
    for row in stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap()
    {
        let (id, name) = row.unwrap();
        lines.push(format!("F|{id}|{name}"));
    }
    let tw = if has_col(conn, "sessions", "transcription_words") {
        "transcription_words"
    } else {
        "NULL"
    };
    let summary = if has_col(conn, "sessions", "summary") {
        "summary"
    } else {
        "NULL"
    };
    let sua = if has_col(conn, "sessions", "summary_updated_at") {
        "summary_updated_at"
    } else {
        "NULL"
    };
    let sql = format!(
        "SELECT id,workspace_id,title,transcription,{summary},{sua},{tw} FROM sessions ORDER BY id"
    );
    let mut stmt = conn.prepare(&sql).unwrap();
    for row in stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        })
        .unwrap()
    {
        let (id, wid, title, transcription, summary, sua, tw) = row.unwrap();
        lines.push(format!(
            "S|{id}|{wid}|{title}|{transcription:?}|{summary:?}|{sua:?}|{tw:?}"
        ));
    }
    let mut stmt = conn
        .prepare("SELECT id,workspace_id,name,content,path FROM materials ORDER BY id")
        .unwrap();
    for row in stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })
        .unwrap()
    {
        let (id, wid, name, content, path) = row.unwrap();
        lines.push(format!("M|{id}|{wid}|{name}|{content}|{path:?}"));
    }
    let created = if has_col(conn, "messages", "created_at") {
        "created_at"
    } else {
        "''"
    };
    let tool_calls = if has_col(conn, "messages", "tool_calls") {
        "tool_calls"
    } else {
        "NULL"
    };
    let sql =
        format!("SELECT id,session_id,position,role,content,{created},{tool_calls} FROM messages ORDER BY id");
    let mut stmt = conn.prepare(&sql).unwrap();
    for row in stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        })
        .unwrap()
    {
        let (id, sid, pos, role, content, created, tool_calls) = row.unwrap();
        lines.push(format!(
            "G|{id}|{sid}|{pos}|{role}|{content}|{created}|{tool_calls:?}"
        ));
    }
    lines.join("\n")
}
/// The same canonical shape, rebuilt from the migrated `nodes`/`sessions`/
/// `materials`/`messages` tables, so it can be compared byte-for-byte against
/// `dump_pre`'s hash.
fn dump_post(conn: &Connection) -> String {
    let mut lines = Vec::new();
    let mut stmt = conn
        .prepare("SELECT id,name FROM nodes WHERE kind='folder' AND parent_id IS NULL ORDER BY id")
        .unwrap();
    for row in stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap()
    {
        let (id, name) = row.unwrap();
        lines.push(format!("F|{id}|{name}"));
    }
    let mut stmt = conn
        .prepare(
            "SELECT n.id,n.parent_id,n.name,s.transcription,s.summary,s.summary_updated_at,s.transcription_words
               FROM nodes n JOIN sessions s ON s.id=n.id WHERE n.kind='session' ORDER BY n.id",
        )
        .unwrap();
    for row in stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        })
        .unwrap()
    {
        let (id, wid, title, transcription, summary, sua, tw) = row.unwrap();
        lines.push(format!(
            "S|{id}|{}|{title}|{transcription:?}|{summary:?}|{sua:?}|{tw:?}",
            wid.unwrap_or_default()
        ));
    }
    let mut stmt = conn
        .prepare(
            "SELECT n.id,n.parent_id,n.name,m.content,m.path FROM nodes n JOIN materials m ON m.id=n.id WHERE n.kind='material' ORDER BY n.id",
        )
        .unwrap();
    for row in stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })
        .unwrap()
    {
        let (id, wid, name, content, path) = row.unwrap();
        lines.push(format!(
            "M|{id}|{}|{name}|{content}|{path:?}",
            wid.unwrap_or_default()
        ));
    }
    let mut stmt = conn
        .prepare("SELECT id,session_id,position,role,content,created_at,tool_calls FROM messages ORDER BY id")
        .unwrap();
    for row in stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        })
        .unwrap()
    {
        let (id, sid, pos, role, content, created, tool_calls) = row.unwrap();
        lines.push(format!(
            "G|{id}|{sid}|{pos}|{role}|{content}|{created}|{tool_calls:?}"
        ));
    }
    lines.join("\n")
}
fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}
fn file_sha256(path: &Path) -> String {
    sha256_hex(&std::fs::read(path).unwrap())
}
fn dump_hash_pre(conn: &Connection) -> String {
    sha256_hex(dump_pre(conn).as_bytes())
}

/// A 36,000-character transcript mixing CJK, English, an emoji, CRLF, and the
/// SQL-special characters `%`, `_`, `'` and `\` - deterministic so tests are
/// reproducible.
fn big_transcript() -> String {
    let mut s = String::new();
    let unit = "第一课：数学 Algebra 100%_off it's a \\test\r\n😀 ";
    while s.len() < 36_000 {
        s.push_str(unit);
    }
    s.truncate(s.floor_char_boundary(36_000));
    s
}

fn seed_live_fixture(conn: &Connection) {
    conn.execute("INSERT INTO workspaces VALUES('w1','Course A')", [])
        .unwrap();
    conn.execute("INSERT INTO workspaces VALUES('w2','Course B')", [])
        .unwrap();
    conn.execute("INSERT INTO workspaces VALUES('w3','Empty Course')", [])
        .unwrap();
    let transcript = big_transcript();
    conn.execute(
        "INSERT INTO sessions(id,workspace_id,title,transcription) VALUES('s1','w1','1',?)",
        params![transcript],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sessions(id,workspace_id,title,transcription,summary,summary_updated_at) VALUES('s2','w2','1','short','notes here','2024-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    for (i, (id, role, content)) in [
        ("m1", "user", "hello"),
        ("m2", "assistant", "hi there"),
        ("m3", "user", "thanks"),
    ]
    .into_iter()
    .enumerate()
    {
        conn.execute(
            "INSERT INTO messages(id,session_id,role,content,position) VALUES(?,?,?,?,?)",
            params![id, "s1", role, content, i as i64],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO materials(id,workspace_id,name,content,path) VALUES('mat1','w1','notes.txt','Some notes','/tmp/notes.txt')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO materials(id,workspace_id,name,content,path) VALUES('mat2','w2','slides.md','# Slides',NULL)",
        [],
    )
    .unwrap();
}

/// Runs `initialize_database` against `path`, asserting the standard
/// successful-migration invariants, and returns the pre-migration dump hash
/// (already asserted equal to the post-migration one) for further checks.
fn run_and_verify_migration(path: &Path, expected_pre_hash: &str) {
    let report = initialize_database(path).expect("migration should succeed");
    let counts = report.migrated.expect("a legacy database must migrate");
    let db = open_connection(path).unwrap();
    let post_hash = sha256_hex(dump_post(&db).as_bytes());
    assert_eq!(
        post_hash, expected_pre_hash,
        "migrated dump must equal source dump"
    );
    let version: i64 = db
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    let integrity: String = db
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    let fk_violations: i64 = db
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(fk_violations, 0);
    let chars: i64 = db
        .query_row(
            "SELECT COALESCE(SUM(length(transcription)),0) FROM sessions",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let bytes: i64 = db
        .query_row(
            "SELECT COALESCE(SUM(length(CAST(transcription AS BLOB))),0) FROM sessions",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(chars, counts.transcript_chars);
    assert_eq!(bytes, counts.transcript_bytes);
}

#[test]
fn legacy_live_shape_migrates_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("async.sqlite3");
    let pre_hash = {
        let conn = open_legacy(&path, LEGACY_LIVE_DDL);
        seed_live_fixture(&conn);
        dump_hash_pre(&conn)
    };
    run_and_verify_migration(&path, &pre_hash);
}

#[test]
fn legacy_full_shape_migrates_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("async.sqlite3");
    let pre_hash = {
        let conn = open_legacy(&path, LEGACY_FULL_DDL);
        conn.execute("INSERT INTO workspaces VALUES('w1','Course A')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO sessions(id,workspace_id,title,transcription,summary,summary_updated_at,transcription_words) VALUES('s1','w1','Lesson','hello world','notes','2024-01-01T00:00:00Z','[{\"word\":\"hi\",\"start\":0.0,\"end\":0.2}]')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages(id,session_id,role,content,position,created_at,tool_calls) VALUES('m1','s1','assistant','answer',0,'2024-01-01T00:00:01Z','[{\"id\":\"c1\",\"name\":\"search_local_materials\",\"status\":\"finished\"}]')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO materials(id,workspace_id,name,content,path) VALUES('mat1','w1','notes.txt','content',NULL)",
            [],
        )
        .unwrap();
        dump_hash_pre(&conn)
    };
    run_and_verify_migration(&path, &pre_hash);
}

#[test]
fn legacy_oldest_shape_migrates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("async.sqlite3");
    let pre_hash = {
        let conn = open_legacy(&path, LEGACY_OLDEST_DDL);
        conn.execute("INSERT INTO workspaces VALUES('w1','Course A')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO sessions(id,workspace_id,title,transcription) VALUES('s1','w1','Lesson','hi')",
            [],
        )
        .unwrap();
        dump_hash_pre(&conn)
    };
    run_and_verify_migration(&path, &pre_hash);
    let db = open_connection(&path).unwrap();
    let sua: Option<String> = db
        .query_row(
            "SELECT summary_updated_at FROM sessions WHERE id='s1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(sua, None);
}

#[test]
fn backup_is_finalized_verified_and_equal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("async.sqlite3");
    let pre_hash = {
        let conn = open_legacy(&path, LEGACY_LIVE_DDL);
        seed_live_fixture(&conn);
        dump_hash_pre(&conn)
    };
    let report = initialize_database(&path).unwrap();
    let backup = report
        .backup
        .expect("a legacy migration must produce a backup");
    assert!(backup.exists());
    assert!(!Path::new(&format!("{}.partial", backup.display())).exists());
    let backups_dir = path.parent().unwrap().join("backups");
    let entries: Vec<_> = std::fs::read_dir(&backups_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(entries.len(), 1, "exactly one backup file: {entries:?}");
    assert!(!entries[0].ends_with(".partial"));
    let backup_conn =
        Connection::open_with_flags(&backup, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let ok: String = backup_conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ok, "ok");
    assert_eq!(dump_hash_pre(&backup_conn), pre_hash);
}

#[test]
fn fresh_install_schema_equals_migrated_schema() {
    let dir = tempfile::tempdir().unwrap();
    let fresh_path = dir.path().join("fresh.sqlite3");
    initialize_database(&fresh_path).unwrap();
    let fresh_db = open_connection(&fresh_path).unwrap();
    let fresh_rows = schema_object_rows(&fresh_db, "").unwrap();

    let migrated_path = dir.path().join("migrated.sqlite3");
    {
        let conn = open_legacy(&migrated_path, LEGACY_LIVE_DDL);
        seed_live_fixture(&conn);
    }
    initialize_database(&migrated_path).unwrap();
    let migrated_db = open_connection(&migrated_path).unwrap();
    let migrated_rows = schema_object_rows(&migrated_db, "").unwrap();

    assert_eq!(fresh_rows, migrated_rows);
}

/// Runs one scenario expected to abort during preflight (before any write),
/// asserting the source is left exactly as it was. Compares a hash of the
/// database file's raw bytes rather than a row dump: preflight is read-only,
/// so the file must come out byte-identical, and a file hash proves that for
/// any column storage type (a row dump can't even read a column back when a
/// scenario deliberately stores the wrong SQLite storage class in it).
fn assert_preflight_aborts(ddl: &str, seed: impl FnOnce(&Connection)) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("async.sqlite3");
    {
        let conn = open_legacy(&path, ddl);
        seed(&conn);
    }
    let pre_hash = file_sha256(&path);
    let err = initialize_database(&path).unwrap_err();
    assert!(!err.message.is_empty());
    assert_eq!(
        file_sha256(&path),
        pre_hash,
        "preflight abort must leave the database file byte-identical"
    );
    let db = open_connection(&path).unwrap();
    let version: i64 = db
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 0);
    assert!(
        has_col(&db, "workspaces", "id"),
        "legacy tables must survive"
    );
    let has_nodes: i64 = db
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='nodes'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(has_nodes, 0);
}

#[test]
fn constraint_violations_roll_back_untouched() {
    assert_preflight_aborts(LEGACY_LIVE_DDL, |conn| {
        conn.execute("INSERT INTO workspaces VALUES('w1','Course A')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO sessions(id,workspace_id,title) VALUES('s1','w1','Lesson')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages(id,session_id,role,content,position) VALUES('m1','s1','user','a',0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages(id,session_id,role,content,position) VALUES('m2','s1','user','b',0)",
            [],
        )
        .unwrap();
    });
    assert_preflight_aborts(LEGACY_LIVE_DDL, |conn| {
        conn.execute("INSERT INTO workspaces VALUES('w1','   ')", [])
            .unwrap();
    });
    assert_preflight_aborts(LEGACY_FULL_DDL, |conn| {
        conn.execute("INSERT INTO workspaces VALUES('w1','Course A')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO sessions(id,workspace_id,title) VALUES('s1','w1','Lesson')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages(id,session_id,role,content,position,tool_calls) VALUES('m1','s1','assistant','a',0,'not json')",
            [],
        )
        .unwrap();
    });
    assert_preflight_aborts(LEGACY_LIVE_DDL, |conn| {
        conn.execute("INSERT INTO workspaces VALUES('w1','Course A')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO sessions(id,workspace_id,title) VALUES('s1','w1','Lesson')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages(id,session_id,role,content,position) VALUES('m1','s1','tool','a',0)",
            [],
        )
        .unwrap();
    });
    assert_preflight_aborts(LEGACY_LIVE_DDL, |conn| {
        conn.execute("INSERT INTO workspaces VALUES('w1','Course A')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO sessions(id,workspace_id,title,transcription) VALUES('s1','w1','Lesson',?)",
            params![vec![1u8, 2, 3]],
        )
        .unwrap();
    });
    assert_preflight_aborts(LEGACY_LIVE_DDL, |conn| {
        conn.execute("INSERT INTO workspaces VALUES('dup','Course A')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO sessions(id,workspace_id,title) VALUES('dup','dup','Lesson')",
            [],
        )
        .unwrap();
    });
}

#[test]
fn injected_failure_after_copy_rolls_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("async.sqlite3");
    let pre_hash = {
        let conn = open_legacy(&path, LEGACY_LIVE_DDL);
        seed_live_fixture(&conn);
        dump_hash_pre(&conn)
    };
    FAIL_AFTER_COPY.store(true, std::sync::atomic::Ordering::SeqCst);
    let result = initialize_database(&path);
    FAIL_AFTER_COPY.store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(result.is_err());
    let db = open_connection(&path).unwrap();
    let version: i64 = db
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 0);
    assert_eq!(dump_hash_pre(&db), pre_hash);
    let has_nodes: i64 = db
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='nodes'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(has_nodes, 0);
}

#[test]
fn orphan_rows_abort() {
    assert_preflight_aborts(LEGACY_LIVE_DDL, |conn| {
        conn.execute("INSERT INTO workspaces VALUES('w1','Course A')", [])
            .unwrap();
        conn.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
        conn.execute(
            "INSERT INTO sessions(id,workspace_id,title) VALUES('s1','missing-workspace','Lesson')",
            [],
        )
        .unwrap();
    });
    assert_preflight_aborts(LEGACY_LIVE_DDL, |conn| {
        conn.execute("INSERT INTO workspaces VALUES('w1','Course A')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO sessions(id,workspace_id,title) VALUES('s1','w1','Lesson')",
            [],
        )
        .unwrap();
        conn.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
        conn.execute(
            "INSERT INTO messages(id,session_id,role,content,position) VALUES('m1','missing-session','user','a',0)",
            [],
        )
        .unwrap();
    });
}

#[test]
fn newer_user_version_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("async.sqlite3");
    {
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 2i64).unwrap();
    }
    let before = file_sha256(&path);
    let err = initialize_database(&path).unwrap_err();
    assert!(err.message.contains("更新版本"));
    assert_eq!(file_sha256(&path), before);
}

#[test]
fn migration_connection_has_foreign_keys_off() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("async.sqlite3");
    {
        let conn = open_legacy(&path, LEGACY_LIVE_DDL);
        seed_live_fixture(&conn);
    }
    initialize_database(&path).unwrap();
    assert_eq!(
        FK_STATE_DURING_TRANSACTION.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

#[test]
fn reopen_is_a_no_op_and_residue_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("async.sqlite3");
    let pre_hash = {
        let conn = open_legacy(&path, LEGACY_LIVE_DDL);
        seed_live_fixture(&conn);
        dump_hash_pre(&conn)
    };
    initialize_database(&path).unwrap();
    let backups_dir = path.parent().unwrap().join("backups");
    let count_after_first = std::fs::read_dir(&backups_dir).unwrap().count();

    let report = initialize_database(&path).unwrap();
    assert!(
        report.migrated.is_none(),
        "a second open must not re-migrate"
    );
    let count_after_second = std::fs::read_dir(&backups_dir).unwrap().count();
    assert_eq!(count_after_first, count_after_second, "no second backup");
    let db = open_connection(&path).unwrap();
    assert_eq!(sha256_hex(dump_post(&db).as_bytes()), pre_hash);

    // An older build sharing this data folder recreates an empty `workspaces`
    // table; it must be ignored (never read, never dropped) and load_state
    // (exercised here as a direct query) must keep working.
    db.execute(
        "CREATE TABLE workspaces(id TEXT PRIMARY KEY, name TEXT NOT NULL)",
        [],
    )
    .unwrap();
    drop(db);
    let report = initialize_database(&path).unwrap();
    assert!(report.migrated.is_none());
    let db = open_connection(&path).unwrap();
    let node_count: i64 = db
        .query_row("SELECT count(*) FROM nodes", [], |r| r.get(0))
        .unwrap();
    assert!(node_count > 0);

    // v0 with a `nodes` table already present is an unrecognized combination.
    let other_path = dir.path().join("weird.sqlite3");
    {
        let conn = Connection::open(&other_path).unwrap();
        conn.execute_batch(SCHEMA_SQL).unwrap();
        // user_version stays 0 here on purpose: the version bump normally
        // commits atomically with the tables, so this combination cannot
        // arise in practice and must be refused explicitly.
    }
    let err = initialize_database(&other_path).unwrap_err();
    assert!(err.message.contains("无法识别"));
}

#[test]
fn backup_collision_aborts_before_write() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("async.sqlite3");
    {
        let conn = open_legacy(&path, LEGACY_LIVE_DDL);
        seed_live_fixture(&conn);
    }
    *TEST_TIMESTAMP.lock().unwrap() = Some("20240101T000000Z".to_string());
    let backups_dir = path.parent().unwrap().join("backups");
    std::fs::create_dir_all(&backups_dir).unwrap();
    std::fs::write(
        backups_dir.join("async-before-node-tree-20240101T000000Z.sqlite3"),
        b"colliding file",
    )
    .unwrap();
    let before = file_sha256(&path);
    let err = initialize_database(&path).unwrap_err();
    *TEST_TIMESTAMP.lock().unwrap() = None;
    assert!(!err.message.is_empty());
    assert_eq!(file_sha256(&path), before, "source must be untouched");
}

/// Rehearses this exact migration against a `VACUUM INTO` copy of the
/// owner's real database, opened only `mode=ro`. No personal values are
/// embedded here; run with `ASYNC_REAL_DB=<path> cargo test rehearse_real_database_copy -- --ignored`.
#[test]
#[ignore = "reads the real database path from ASYNC_REAL_DB; run manually before merge"]
fn rehearse_real_database_copy() {
    let real_path = std::env::var("ASYNC_REAL_DB").expect("set ASYNC_REAL_DB to the real db path");
    let source =
        Connection::open_with_flags(&real_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("open the real database read-only");
    let expected = (
        source
            .query_row("SELECT count(*) FROM workspaces", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap(),
        source
            .query_row("SELECT count(*) FROM sessions", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        source
            .query_row("SELECT count(*) FROM materials", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        source
            .query_row("SELECT count(*) FROM messages", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        source
            .query_row(
                "SELECT COALESCE(SUM(length(transcription)),0) FROM sessions",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap(),
        source
            .query_row(
                "SELECT COALESCE(SUM(length(CAST(transcription AS BLOB))),0) FROM sessions",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap(),
    );
    let dir = tempfile::tempdir().unwrap();
    let copy_path = dir.path().join("rehearsal.sqlite3");
    source
        .execute("VACUUM INTO ?1", [copy_path.to_str().unwrap()])
        .unwrap();
    drop(source);
    let report = initialize_database(&copy_path).expect("migration must succeed on the real shape");
    let counts = report.migrated.expect("the real file is a legacy database");
    assert_eq!(
        (
            counts.folders,
            counts.sessions,
            counts.materials,
            counts.messages,
            counts.transcript_chars,
            counts.transcript_bytes,
        ),
        expected
    );
}

/// Deeper rehearsal than test 12: runs the real entry point twice, in place,
/// on a writable copy of the real database, and proves the second run is a
/// byte-for-byte no-op. Run with
/// `ASYNC_REHEARSAL_DB=<writable copy path> cargo test rehearse_real_database_is_lossless_and_idempotent -- --ignored`.
#[test]
#[ignore = "mutates a writable copy of the real database path from ASYNC_REHEARSAL_DB; run manually before merge"]
fn rehearse_real_database_is_lossless_and_idempotent() {
    let path_str = std::env::var("ASYNC_REHEARSAL_DB")
        .expect("set ASYNC_REHEARSAL_DB to a writable copy of the real database");
    let path = Path::new(&path_str);

    let (expected, pre_hash) = {
        let conn = Connection::open(path).unwrap();
        (read_counts(&conn).unwrap(), dump_hash_pre(&conn))
    };
    let sha_before = file_sha256(path);

    let report1 =
        initialize_database(path).expect("first migration must succeed on the real shape");
    let counts1 = report1
        .migrated
        .expect("the real file is a legacy database");
    assert_eq!(counts1, expected);

    let db = open_connection(path).unwrap();
    assert_eq!(
        sha256_hex(dump_post(&db).as_bytes()),
        pre_hash,
        "migrated content must match the pre-migration dump exactly"
    );
    let version: i64 = db
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    let integrity: String = db
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    let fk_violations: i64 = db
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(fk_violations, 0);
    drop(db);

    let backup = report1.backup.expect("a backup must have been recorded");
    assert!(backup.exists(), "backup file must exist on disk");
    let backup_conn =
        Connection::open_with_flags(&backup, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let backup_ok: String = backup_conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(backup_ok, "ok");
    assert_eq!(
        dump_hash_pre(&backup_conn),
        pre_hash,
        "backup must hold the exact pre-migration content"
    );
    drop(backup_conn);

    let backups_dir = backup.parent().unwrap().to_path_buf();
    let count_after_first = std::fs::read_dir(&backups_dir).unwrap().count();
    let sha_after_first = file_sha256(path);

    let report2 = initialize_database(path).expect("second run must succeed");
    assert!(
        report2.migrated.is_none(),
        "a second run must not re-migrate"
    );
    let count_after_second = std::fs::read_dir(&backups_dir).unwrap().count();
    assert_eq!(count_after_first, count_after_second, "no second backup");
    let sha_after_second = file_sha256(path);
    assert_eq!(
        sha_after_first, sha_after_second,
        "a second run must be a byte-for-byte no-op"
    );

    eprintln!(
        "rehearsal ok: sha256 before migration {sha_before}, after first run {sha_after_first}, after second run {sha_after_second}"
    );
}

/// Proves the migration refuses a session whose workspace_id points nowhere,
/// leaving the file byte-identical. Run with
/// `ASYNC_REHEARSAL_ORPHAN_DB=<writable copy path> cargo test rehearse_real_database_rejects_orphan_session -- --ignored`.
#[test]
#[ignore = "mutates a writable copy of the real database path from ASYNC_REHEARSAL_ORPHAN_DB; run manually before merge"]
fn rehearse_real_database_rejects_orphan_session() {
    let path_str = std::env::var("ASYNC_REHEARSAL_ORPHAN_DB")
        .expect("set ASYNC_REHEARSAL_ORPHAN_DB to a writable copy of the real database");
    let path = Path::new(&path_str);

    {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
        conn.execute(
            "INSERT INTO sessions(id,workspace_id,title) VALUES(?,?,?)",
            params![
                "rehearsal-orphan-session",
                "rehearsal-no-such-workspace",
                "Orphan"
            ],
        )
        .unwrap();
    }
    let sha_before = file_sha256(path);

    let err = initialize_database(path).expect_err("an orphan session must abort the migration");
    assert!(
        err.message.contains("无法迁移"),
        "unexpected error message: {}",
        err.message
    );

    let sha_after = file_sha256(path);
    assert_eq!(
        sha_before, sha_after,
        "an aborted migration must leave the file byte-identical"
    );
}

/// Replays the copy step with the session summary and summary_updated_at
/// values swapped - the exact class of column-mapping mistake
/// verify_migration's IS-joins exist to catch - then asserts verify_migration
/// itself rejects it. Before the fix, verify_migration never compared those
/// columns against the backup at all, so this same swap would pass silently.
#[test]
fn verify_migration_catches_a_mismapped_summary_column() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("async.sqlite3");
    let mut conn = open_legacy(&path, LEGACY_FULL_DDL);
    conn.execute("INSERT INTO workspaces VALUES('w1','Course A')", [])
        .unwrap();
    conn.execute(
        "INSERT INTO sessions(id,workspace_id,title,transcription,summary,summary_updated_at,transcription_words) VALUES('s1','w1','Lesson','hi','right-summary','2024-01-01T00:00:00Z',NULL)",
        [],
    )
    .unwrap();

    let legacy = introspect_legacy_columns(&conn).unwrap();
    let expected = preflight(&conn, &legacy).unwrap();
    let backup = create_verified_backup(&conn, &path, expected).unwrap();
    conn.execute("ATTACH DATABASE ?1 AS pre", [readonly_uri(&backup)])
        .unwrap();

    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Exclusive)
        .unwrap();
    tx.execute_batch(
        "ALTER TABLE messages RENAME TO legacy_messages;
         ALTER TABLE sessions RENAME TO legacy_sessions;
         ALTER TABLE materials RENAME TO legacy_materials;
         ALTER TABLE workspaces RENAME TO legacy_workspaces;",
    )
    .unwrap();
    tx.execute_batch(SCHEMA_SQL).unwrap();
    tx.execute_batch(
        "INSERT INTO main.nodes(id,parent_id,parent_kind,kind,name)
           SELECT id,NULL,NULL,'folder',name FROM legacy_workspaces;
         INSERT INTO main.nodes(id,parent_id,parent_kind,kind,name)
           SELECT id,workspace_id,'folder','session',title FROM legacy_sessions;",
    )
    .unwrap();
    // Deliberately swapped, reproducing a col_or expression mix-up: this must
    // be caught by the session IS-join, not slip through as a silent commit.
    tx.execute_batch(
        "INSERT INTO main.sessions(id,transcription,transcription_words,summary,summary_updated_at,notes_enabled,translation_enabled,translation_target_language,translation_mode)
           SELECT id,transcription,transcription_words,summary_updated_at,summary,1,0,NULL,'side-by-side' FROM legacy_sessions;",
    )
    .unwrap();

    let err = verify_migration(&tx, &legacy, expected).unwrap_err();
    assert!(
        err.contains("会话"),
        "expected the session mismatch error, got: {err}"
    );
}

//! Node-tree schema, fresh-install creation and the one-time legacy migration.
//!
//! DEVIATIONS FROM THE APPROVED SPEC (agents/008.../spec.md), both because
//! notes/translation landed in this codebase after the spec was written:
//! - `sessions` gains notes_enabled/translation_enabled/translation_target_language/
//!   translation_mode columns (today's `ensure_column` defaults), and the legacy
//!   column whitelist and migration copy/verify steps are extended the same way
//!   the spec extends summary/summary_updated_at/transcription_words.
//! - `sentence_translations` (the live-translation cache) is unrelated to the
//!   node tree; it is created in STEP 7 next to providers/settings, unchanged.
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub const SCHEMA_VERSION: i64 = 1;

pub const SCHEMA_SQL: &str = "
CREATE TABLE nodes(
  id TEXT PRIMARY KEY NOT NULL,
  parent_id TEXT,
  parent_kind TEXT CHECK(parent_kind = 'folder'),
  kind TEXT NOT NULL CHECK(kind IN ('folder','session','material')),
  name TEXT NOT NULL CHECK(trim(name) <> ''),
  UNIQUE(id, kind),
  CHECK((parent_id IS NULL) = (parent_kind IS NULL)),
  CHECK(parent_id IS NULL OR parent_id <> id),
  FOREIGN KEY(parent_id, parent_kind) REFERENCES nodes(id, kind) ON DELETE CASCADE
) STRICT;
CREATE INDEX nodes_parent ON nodes(parent_id, parent_kind);
CREATE TABLE sessions(
  id TEXT PRIMARY KEY NOT NULL,
  kind TEXT NOT NULL DEFAULT 'session' CHECK(kind = 'session'),
  transcription TEXT,
  transcription_words TEXT CHECK(transcription_words IS NULL OR json_valid(transcription_words)),
  summary TEXT,
  summary_updated_at TEXT,
  notes_enabled INTEGER NOT NULL DEFAULT 1 CHECK(notes_enabled IN (0,1)),
  translation_enabled INTEGER NOT NULL DEFAULT 0 CHECK(translation_enabled IN (0,1)),
  translation_target_language TEXT,
  translation_mode TEXT NOT NULL DEFAULT 'side-by-side' CHECK(translation_mode IN ('side-by-side','separate')),
  FOREIGN KEY(id, kind) REFERENCES nodes(id, kind) ON DELETE CASCADE
) STRICT;
CREATE TABLE materials(
  id TEXT PRIMARY KEY NOT NULL,
  kind TEXT NOT NULL DEFAULT 'material' CHECK(kind = 'material'),
  content TEXT NOT NULL,
  path TEXT,
  FOREIGN KEY(id, kind) REFERENCES nodes(id, kind) ON DELETE CASCADE
) STRICT;
CREATE TABLE messages(
  id TEXT PRIMARY KEY NOT NULL,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  position INTEGER NOT NULL CHECK(position >= 0),
  role TEXT NOT NULL CHECK(role IN ('user','assistant','system')),
  content TEXT NOT NULL,
  created_at TEXT NOT NULL,
  tool_calls TEXT CHECK(tool_calls IS NULL OR json_valid(tool_calls)),
  UNIQUE(session_id, position)
) STRICT;
CREATE TRIGGER nodes_kind_immutable BEFORE UPDATE OF kind ON nodes
WHEN NEW.kind IS NOT OLD.kind
BEGIN SELECT RAISE(ABORT, 'node kind is immutable'); END;
CREATE TRIGGER nodes_no_cycle BEFORE UPDATE OF parent_id ON nodes
WHEN NEW.parent_id IS NOT NULL
BEGIN
  SELECT RAISE(ABORT, 'node move would create a cycle')
  WHERE EXISTS (
    WITH RECURSIVE chain(id) AS (
      SELECT NEW.parent_id
      UNION
      SELECT n.parent_id FROM nodes n JOIN chain c ON n.id = c.id WHERE n.parent_id IS NOT NULL
    ) SELECT 1 FROM chain WHERE chain.id = NEW.id
  );
END;
CREATE TRIGGER sessions_payload_bound BEFORE DELETE ON sessions
WHEN EXISTS (SELECT 1 FROM nodes WHERE id = OLD.id)
BEGIN SELECT RAISE(ABORT, 'session payload is deleted only with its node'); END;
CREATE TRIGGER materials_payload_bound BEFORE DELETE ON materials
WHEN EXISTS (SELECT 1 FROM nodes WHERE id = OLD.id)
BEGIN SELECT RAISE(ABORT, 'material payload is deleted only with its node'); END;
";

const SCHEMA_OBJECT_NAMES: [&str; 9] = [
    "materials",
    "materials_payload_bound",
    "messages",
    "nodes",
    "nodes_kind_immutable",
    "nodes_no_cycle",
    "nodes_parent",
    "sessions",
    "sessions_payload_bound",
];

/// Startup failure: `backup` is set once a verified backup exists on disk, so
/// the caller can tell the owner where their data was preserved.
#[derive(Debug, Clone)]
pub struct InitError {
    pub message: String,
    pub backup: Option<PathBuf>,
}
impl InitError {
    fn simple(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            backup: None,
        }
    }
    fn with_backup(message: impl Into<String>, backup: PathBuf) -> Self {
        Self {
            message: message.into(),
            backup: Some(backup),
        }
    }
}
impl From<String> for InitError {
    fn from(message: String) -> Self {
        Self::simple(message)
    }
}
impl std::fmt::Display for InitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MigrationCounts {
    pub folders: i64,
    pub sessions: i64,
    pub materials: i64,
    pub messages: i64,
    pub transcript_chars: i64,
    pub transcript_bytes: i64,
}

// Read by tests and available for a future startup notice; `initialize_database`
// itself already logs the same facts via `eprintln!`.
#[derive(Debug, Default)]
#[allow(dead_code)]
pub struct InitReport {
    pub migrated: Option<MigrationCounts>,
    pub backup: Option<PathBuf>,
}

/// Test-only hook: set to force STEP 4 to fail right after the copy (before
/// verification), proving the transaction rolls back with nothing half-applied.
#[cfg(test)]
pub(crate) static FAIL_AFTER_COPY: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
/// Test-only hook: overrides the backup filename's timestamp so a test can
/// deterministically pre-create a colliding backup file name.
#[cfg(test)]
pub(crate) static TEST_TIMESTAMP: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
/// Test-only probe: `migrate_within_transaction` records `PRAGMA foreign_keys`
/// as read inside its own transaction, so a test can assert it stayed off
/// (guarding against `DROP TABLE` cascading during the legacy-table cleanup).
#[cfg(test)]
pub(crate) static FK_STATE_DURING_TRANSACTION: std::sync::atomic::AtomicI64 =
    std::sync::atomic::AtomicI64::new(-1);

fn backup_timestamp() -> String {
    #[cfg(test)]
    {
        if let Ok(guard) = TEST_TIMESTAMP.lock() {
            if let Some(value) = guard.as_ref() {
                return value.clone();
            }
        }
    }
    chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

/// Opens a connection for ordinary command use: no DDL, no seeding. Schema
/// creation happens only in `initialize_database`, called once from
/// `AppState::open`.
pub fn open_connection(path: &Path) -> Result<Connection, String> {
    let db = Connection::open(path).map_err(|e| e.to_string())?;
    db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;")
        .map_err(|e| e.to_string())?;
    Ok(db)
}

fn table_columns(db: &Connection, table: &str) -> Result<Vec<String>, String> {
    db.prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
        .map_err(|e| e.to_string())?
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}
fn has_column(cols: &[String], name: &str) -> bool {
    cols.iter().any(|c| c == name)
}
/// A present column's own name, or the literal expression when the column
/// predates it in the legacy database (the whitelist is fixed at compile
/// time; it is never built from table data).
fn col_or(cols: &[String], name: &str, default_literal: &str) -> String {
    if has_column(cols, name) {
        name.to_string()
    } else {
        default_literal.to_string()
    }
}
fn require_columns(db: &Connection, table: &str, required: &[&str]) -> Result<Vec<String>, String> {
    let exists: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name=?",
            [table],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if exists == 0 {
        return Err(format!("旧数据库缺少 {table} 表，无法迁移。"));
    }
    let cols = table_columns(db, table)?;
    for column in required {
        if !has_column(&cols, column) {
            return Err(format!("旧数据库缺少 {table}.{column} 列，无法迁移。"));
        }
    }
    Ok(cols)
}

fn list_tables(db: &Connection) -> Result<Vec<String>, String> {
    db.prepare("SELECT name FROM sqlite_schema WHERE type='table'")
        .map_err(|e| e.to_string())?
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

fn check_typeof(
    db: &Connection,
    table: &str,
    column: &str,
    allowed: &[&str],
) -> Result<(), String> {
    let predicate = allowed
        .iter()
        .map(|a| format!("'{a}'"))
        .collect::<Vec<_>>()
        .join(",");
    let bad: Option<(i64, String)> = db
        .query_row(
            &format!(
                "SELECT rowid, typeof({column}) FROM {table} WHERE typeof({column}) NOT IN ({predicate}) ORDER BY rowid LIMIT 1"
            ),
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    if let Some((rowid, ty)) = bad {
        return Err(format!(
            "第 {rowid} 行的 {table}.{column} 存储类型为 {ty}，无法迁移。"
        ));
    }
    Ok(())
}
fn check_blank(db: &Connection, table: &str, column: &str) -> Result<(), String> {
    let ids: Vec<String> = db
        .prepare(&format!(
            "SELECT id FROM {table} WHERE trim({column})='' LIMIT 20"
        ))
        .map_err(|e| e.to_string())?
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    if ids.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{table}.{column} 存在空白名称，无法迁移：{}",
            ids.join(", ")
        ))
    }
}
fn check_orphans(db: &Connection, sql: &str, message: &str) -> Result<(), String> {
    let ids: Vec<String> = db
        .prepare(sql)
        .map_err(|e| e.to_string())?
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    if ids.is_empty() {
        Ok(())
    } else {
        Err(format!("{message}：{}", ids.join(", ")))
    }
}

struct LegacyColumns {
    summary: String,
    summary_updated_at: String,
    transcription_words: String,
    notes_enabled: String,
    translation_enabled: String,
    translation_target_language: String,
    translation_mode: String,
    created_at: String,
    tool_calls: String,
    sessions_cols: Vec<String>,
    messages_cols: Vec<String>,
}

/// STEP 0 legacy column introspection (whitelisted; never built from table
/// data) plus the required-column check.
fn introspect_legacy_columns(db: &Connection) -> Result<LegacyColumns, String> {
    require_columns(db, "workspaces", &["id", "name"])?;
    require_columns(
        db,
        "materials",
        &["id", "workspace_id", "name", "content", "path"],
    )?;
    let messages_cols = require_columns(
        db,
        "messages",
        &["id", "session_id", "role", "content", "position"],
    )?;
    let sessions_cols = require_columns(
        db,
        "sessions",
        &["id", "workspace_id", "title", "transcription"],
    )?;
    Ok(LegacyColumns {
        summary: col_or(&sessions_cols, "summary", "NULL"),
        summary_updated_at: col_or(&sessions_cols, "summary_updated_at", "NULL"),
        transcription_words: col_or(&sessions_cols, "transcription_words", "NULL"),
        notes_enabled: col_or(&sessions_cols, "notes_enabled", "1"),
        translation_enabled: col_or(&sessions_cols, "translation_enabled", "0"),
        translation_target_language: col_or(&sessions_cols, "translation_target_language", "NULL"),
        translation_mode: col_or(&sessions_cols, "translation_mode", "'side-by-side'"),
        created_at: col_or(&messages_cols, "created_at", "''"),
        tool_calls: col_or(&messages_cols, "tool_calls", "NULL"),
        sessions_cols,
        messages_cols,
    })
}

/// STEP 1 preflight: read-only. Every failure aborts before any write.
fn preflight(db: &Connection, legacy: &LegacyColumns) -> Result<MigrationCounts, String> {
    let ok: String = db
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if ok != "ok" {
        return Err(format!("数据库完整性检查失败：{ok}"));
    }
    check_orphans(
        db,
        "SELECT s.id FROM sessions s LEFT JOIN workspaces w ON w.id=s.workspace_id WHERE w.id IS NULL LIMIT 20",
        "存在没有所属工作区的会话，无法迁移",
    )?;
    check_orphans(
        db,
        "SELECT m.id FROM materials m LEFT JOIN workspaces w ON w.id=m.workspace_id WHERE w.id IS NULL LIMIT 20",
        "存在没有所属工作区的材料，无法迁移",
    )?;
    check_orphans(
        db,
        "SELECT m.id FROM messages m LEFT JOIN sessions s ON s.id=m.session_id WHERE s.id IS NULL LIMIT 20",
        "存在没有所属会话的消息，无法迁移",
    )?;
    let fk_violations: i64 = db
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .map_err(|e| e.to_string())?;
    if fk_violations != 0 {
        return Err("外键检查发现违例，无法迁移。".into());
    }

    check_typeof(db, "workspaces", "id", &["text"])?;
    check_typeof(db, "workspaces", "name", &["text"])?;
    check_typeof(db, "sessions", "id", &["text"])?;
    check_typeof(db, "sessions", "workspace_id", &["text"])?;
    check_typeof(db, "sessions", "title", &["text"])?;
    check_typeof(db, "sessions", "transcription", &["text", "null"])?;
    check_typeof(db, "materials", "id", &["text"])?;
    check_typeof(db, "materials", "workspace_id", &["text"])?;
    check_typeof(db, "materials", "name", &["text"])?;
    check_typeof(db, "materials", "content", &["text"])?;
    check_typeof(db, "materials", "path", &["text", "null"])?;
    check_typeof(db, "messages", "id", &["text"])?;
    check_typeof(db, "messages", "session_id", &["text"])?;
    check_typeof(db, "messages", "role", &["text"])?;
    check_typeof(db, "messages", "content", &["text"])?;
    check_typeof(db, "messages", "position", &["integer"])?;
    if has_column(&legacy.sessions_cols, "summary") {
        check_typeof(db, "sessions", "summary", &["text", "null"])?;
    }
    if has_column(&legacy.sessions_cols, "summary_updated_at") {
        check_typeof(db, "sessions", "summary_updated_at", &["text", "null"])?;
    }
    if has_column(&legacy.sessions_cols, "transcription_words") {
        check_typeof(db, "sessions", "transcription_words", &["text", "null"])?;
    }
    if has_column(&legacy.sessions_cols, "notes_enabled") {
        check_typeof(db, "sessions", "notes_enabled", &["integer"])?;
    }
    if has_column(&legacy.sessions_cols, "translation_enabled") {
        check_typeof(db, "sessions", "translation_enabled", &["integer"])?;
    }
    if has_column(&legacy.sessions_cols, "translation_target_language") {
        check_typeof(
            db,
            "sessions",
            "translation_target_language",
            &["text", "null"],
        )?;
    }
    if has_column(&legacy.sessions_cols, "translation_mode") {
        check_typeof(db, "sessions", "translation_mode", &["text"])?;
    }
    if has_column(&legacy.messages_cols, "created_at") {
        check_typeof(db, "messages", "created_at", &["text"])?;
    }
    if has_column(&legacy.messages_cols, "tool_calls") {
        check_typeof(db, "messages", "tool_calls", &["text", "null"])?;
    }

    check_blank(db, "workspaces", "name")?;
    check_blank(db, "sessions", "title")?;
    check_blank(db, "materials", "name")?;
    let bad_role: i64 = db
        .query_row(
            "SELECT count(*) FROM messages WHERE role NOT IN ('user','assistant','system')",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if bad_role != 0 {
        return Err("messages.role 存在非法取值，无法迁移。".into());
    }
    let bad_position: i64 = db
        .query_row(
            "SELECT count(*) FROM messages WHERE position < 0",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if bad_position != 0 {
        return Err("messages.position 存在负值，无法迁移。".into());
    }
    let dup_position: i64 = db
        .query_row(
            "SELECT count(*) FROM (SELECT session_id,position FROM messages GROUP BY session_id,position HAVING count(*)>1)",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if dup_position != 0 {
        return Err("messages 存在重复的 (session_id, position)，无法迁移。".into());
    }
    if has_column(&legacy.sessions_cols, "transcription_words") {
        let bad_json: i64 = db
            .query_row(
                "SELECT count(*) FROM sessions WHERE transcription_words IS NOT NULL AND NOT json_valid(transcription_words)",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if bad_json != 0 {
            return Err("sessions.transcription_words 存在无效 JSON，无法迁移。".into());
        }
    }
    if has_column(&legacy.messages_cols, "tool_calls") {
        let bad_json: i64 = db
            .query_row(
                "SELECT count(*) FROM messages WHERE tool_calls IS NOT NULL AND NOT json_valid(tool_calls)",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if bad_json != 0 {
            return Err("messages.tool_calls 存在无效 JSON，无法迁移。".into());
        }
    }
    let dup_id: i64 = db
        .query_row(
            "SELECT count(*) FROM (SELECT id FROM (SELECT id FROM workspaces UNION ALL SELECT id FROM sessions UNION ALL SELECT id FROM materials) GROUP BY id HAVING count(*)>1)",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if dup_id != 0 {
        return Err("同一 id 在多张表中重复出现，无法迁移。".into());
    }

    read_counts(db)
}

fn read_counts(db: &Connection) -> Result<MigrationCounts, String> {
    let folders: i64 = db
        .query_row("SELECT count(*) FROM workspaces", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let sessions: i64 = db
        .query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let materials: i64 = db
        .query_row("SELECT count(*) FROM materials", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let messages: i64 = db
        .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let transcript_chars: i64 = db
        .query_row(
            "SELECT COALESCE(SUM(length(transcription)),0) FROM sessions",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let transcript_bytes: i64 = db
        .query_row(
            "SELECT COALESCE(SUM(length(CAST(transcription AS BLOB))),0) FROM sessions",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    Ok(MigrationCounts {
        folders,
        sessions,
        materials,
        messages,
        transcript_chars,
        transcript_bytes,
    })
}

fn percent_encode_path(path: &Path) -> String {
    #[allow(unused_mut)]
    let mut s = path.to_string_lossy().to_string();
    #[cfg(windows)]
    {
        s = s.replace('\\', "/");
        let bytes = s.as_bytes();
        if bytes.len() >= 2 && bytes[1] == b':' {
            s = format!("/{s}");
        }
    }
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '%' => out.push_str("%25"),
            '?' => out.push_str("%3F"),
            '#' => out.push_str("%23"),
            ' ' => out.push_str("%20"),
            _ => out.push(ch),
        }
    }
    out
}
fn readonly_uri(path: &Path) -> String {
    format!("file:{}?mode=ro", percent_encode_path(path))
}

/// STEP 2: a verified, fsynced backup, written before the source is touched.
fn create_verified_backup(
    db: &Connection,
    source_path: &Path,
    expected: MigrationCounts,
) -> Result<PathBuf, InitError> {
    let dir = source_path
        .parent()
        .ok_or_else(|| InitError::simple("本地数据路径无效。"))?
        .join("backups");
    fs::create_dir_all(&dir).map_err(|e| InitError::simple(e.to_string()))?;
    let timestamp = backup_timestamp();
    let final_path = dir.join(format!("async-before-node-tree-{timestamp}.sqlite3"));
    let partial_path = dir.join(format!(
        "async-before-node-tree-{timestamp}.sqlite3.partial"
    ));
    if final_path.exists() || partial_path.exists() {
        return Err(InitError::simple(
            "备份文件已存在，已停止启动，数据未改动。",
        ));
    }
    let partial_str = partial_path
        .to_str()
        .ok_or_else(|| InitError::simple("备份路径包含无效字符。"))?;
    db.execute("VACUUM INTO ?1", [partial_str])
        .map_err(|e| InitError::simple(format!("创建备份失败：{e}")))?;
    fs::File::open(&partial_path)
        .and_then(|f| f.sync_all())
        .map_err(|e| InitError::simple(format!("备份落盘失败：{e}")))?;
    fs::rename(&partial_path, &final_path).map_err(|e| InitError::simple(e.to_string()))?;
    #[cfg(unix)]
    {
        fs::File::open(&dir)
            .and_then(|f| f.sync_all())
            .map_err(|e| InitError::simple(e.to_string()))?;
    }
    let check = Connection::open_with_flags(&final_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| InitError::with_backup(e.to_string(), final_path.clone()))?;
    let ok: String = check
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .map_err(|e| InitError::with_backup(e.to_string(), final_path.clone()))?;
    if ok != "ok" {
        return Err(InitError::with_backup(
            format!("备份完整性检查失败：{ok}"),
            final_path,
        ));
    }
    let actual = read_counts(&check).map_err(|e| InitError::with_backup(e, final_path.clone()))?;
    if actual != expected {
        return Err(InitError::with_backup(
            "备份内容与源数据库不一致，已停止启动。",
            final_path,
        ));
    }
    Ok(final_path)
}

fn schema_object_rows(
    db: &Connection,
    prefix: &str,
) -> Result<Vec<(String, String, String)>, String> {
    let sql = format!(
        "SELECT type,name,sql FROM {prefix}sqlite_schema WHERE name IN ({}) ORDER BY name",
        SCHEMA_OBJECT_NAMES
            .iter()
            .map(|n| format!("'{n}'"))
            .collect::<Vec<_>>()
            .join(",")
    );
    db.prepare(&sql)
        .map_err(|e| e.to_string())?
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

/// STEP 4: rename legacy tables aside, create the new tables from SCHEMA_SQL,
/// copy every row across in one exclusive transaction, verify against the
/// attached read-only backup, then commit.
fn migrate_within_transaction(
    db: &mut Connection,
    legacy: &LegacyColumns,
    expected: MigrationCounts,
) -> Result<(), String> {
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Exclusive)
        .map_err(|e| e.to_string())?;
    #[cfg(test)]
    {
        let fk: i64 = tx
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap_or(-1);
        FK_STATE_DURING_TRANSACTION.store(fk, std::sync::atomic::Ordering::SeqCst);
    }
    tx.execute_batch(
        "ALTER TABLE messages RENAME TO legacy_messages;
         ALTER TABLE sessions RENAME TO legacy_sessions;
         ALTER TABLE materials RENAME TO legacy_materials;
         ALTER TABLE workspaces RENAME TO legacy_workspaces;",
    )
    .map_err(|e| e.to_string())?;
    tx.execute_batch(SCHEMA_SQL).map_err(|e| e.to_string())?;
    tx.execute_batch(
        "INSERT INTO main.nodes(id,parent_id,parent_kind,kind,name)
           SELECT id,NULL,NULL,'folder',name FROM legacy_workspaces ORDER BY rowid;
         INSERT INTO main.nodes(id,parent_id,parent_kind,kind,name)
           SELECT id,workspace_id,'folder','session',title FROM legacy_sessions ORDER BY rowid;
         INSERT INTO main.nodes(id,parent_id,parent_kind,kind,name)
           SELECT id,workspace_id,'folder','material',name FROM legacy_materials ORDER BY rowid;",
    )
    .map_err(|e| e.to_string())?;
    tx.execute_batch(&format!(
        "INSERT INTO main.sessions(id,transcription,transcription_words,summary,summary_updated_at,notes_enabled,translation_enabled,translation_target_language,translation_mode)
           SELECT id,transcription,{tw},{summary},{sua},{notes},{trenabled},{trlang},{trmode} FROM legacy_sessions ORDER BY rowid;",
        tw = legacy.transcription_words,
        summary = legacy.summary,
        sua = legacy.summary_updated_at,
        notes = legacy.notes_enabled,
        trenabled = legacy.translation_enabled,
        trlang = legacy.translation_target_language,
        trmode = legacy.translation_mode,
    ))
    .map_err(|e| e.to_string())?;
    tx.execute_batch(
        "INSERT INTO main.materials(id,content,path) SELECT id,content,path FROM legacy_materials ORDER BY rowid;",
    )
    .map_err(|e| e.to_string())?;
    tx.execute_batch(&format!(
        "INSERT INTO main.messages(id,session_id,position,role,content,created_at,tool_calls)
           SELECT id,session_id,position,role,content,{created_at},{tool_calls} FROM legacy_messages ORDER BY rowid;",
        created_at = legacy.created_at,
        tool_calls = legacy.tool_calls,
    ))
    .map_err(|e| e.to_string())?;
    tx.execute_batch(
        "DROP TABLE legacy_messages; DROP TABLE legacy_sessions; DROP TABLE legacy_materials; DROP TABLE legacy_workspaces;",
    )
    .map_err(|e| e.to_string())?;

    #[cfg(test)]
    if FAIL_AFTER_COPY.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("injected test failure after copy".into());
    }

    verify_migration(&tx, expected)?;

    let fk_violations: i64 = tx
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .map_err(|e| e.to_string())?;
    if fk_violations != 0 {
        return Err("迁移后外键检查发现违例。".into());
    }

    let fresh = Connection::open_in_memory().map_err(|e| e.to_string())?;
    fresh.execute_batch(SCHEMA_SQL).map_err(|e| e.to_string())?;
    let fresh_rows = schema_object_rows(&fresh, "")?;
    let migrated_rows = schema_object_rows(&tx, "main.")?;
    if fresh_rows != migrated_rows {
        return Err("迁移后的表结构与全新安装不一致。".into());
    }

    tx.pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

fn verify_migration(tx: &Connection, expected: MigrationCounts) -> Result<(), String> {
    let MigrationCounts {
        folders,
        sessions,
        materials,
        messages,
        transcript_chars,
        transcript_bytes,
    } = expected;

    let folder_count: i64 = tx
        .query_row(
            "SELECT count(*) FROM main.nodes WHERE kind='folder' AND parent_id IS NULL",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let folder_match: i64 = tx
        .query_row(
            "SELECT count(*) FROM pre.workspaces w JOIN main.nodes n ON n.id=w.id
               WHERE n.kind='folder' AND n.parent_id IS NULL AND n.name IS w.name",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if folder_count != folders || folder_match != folders {
        return Err("迁移后的文件夹数量或内容与源数据不一致。".into());
    }

    let session_match: i64 = tx
        .query_row(
            "SELECT count(*) FROM pre.sessions o
               JOIN main.nodes n ON n.id=o.id
               JOIN main.sessions s ON s.id=o.id
               WHERE n.kind='session' AND n.parent_id IS o.workspace_id AND n.parent_kind='folder'
                 AND n.name IS o.title AND s.transcription IS o.transcription",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let session_count: i64 = tx
        .query_row("SELECT count(*) FROM main.sessions", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let session_node_count: i64 = tx
        .query_row(
            "SELECT count(*) FROM main.nodes WHERE kind='session'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if session_match != sessions || session_count != sessions || session_node_count != sessions {
        return Err("迁移后的会话数量或内容与源数据不一致。".into());
    }

    let material_match: i64 = tx
        .query_row(
            "SELECT count(*) FROM pre.materials o
               JOIN main.nodes n ON n.id=o.id
               JOIN main.materials m ON m.id=o.id
               WHERE n.kind='material' AND n.parent_id IS o.workspace_id AND n.parent_kind='folder'
                 AND n.name IS o.name AND m.content IS o.content AND m.path IS o.path",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let material_count: i64 = tx
        .query_row("SELECT count(*) FROM main.materials", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let material_node_count: i64 = tx
        .query_row(
            "SELECT count(*) FROM main.nodes WHERE kind='material'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if material_match != materials
        || material_count != materials
        || material_node_count != materials
    {
        return Err("迁移后的材料数量或内容与源数据不一致。".into());
    }

    let message_match: i64 = tx
        .query_row(
            "SELECT count(*) FROM pre.messages o
               JOIN main.messages m ON m.id=o.id
               WHERE m.session_id IS o.session_id AND m.position IS o.position
                 AND m.role IS o.role AND m.content IS o.content",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if message_match != messages {
        return Err("迁移后的消息数量或内容与源数据不一致。".into());
    }

    let total_nodes: i64 = tx
        .query_row("SELECT count(*) FROM main.nodes", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if total_nodes != folders + sessions + materials {
        return Err("迁移后的节点总数与源数据不一致。".into());
    }

    let orphan_session_nodes: i64 = tx
        .query_row(
            "SELECT count(*) FROM main.nodes n LEFT JOIN main.sessions s ON s.id=n.id WHERE n.kind='session' AND s.id IS NULL",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let orphan_material_nodes: i64 = tx
        .query_row(
            "SELECT count(*) FROM main.nodes n LEFT JOIN main.materials m ON m.id=n.id WHERE n.kind='material' AND m.id IS NULL",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if orphan_session_nodes != 0 || orphan_material_nodes != 0 {
        return Err("迁移后存在没有对应负载的节点。".into());
    }

    let chars: i64 = tx
        .query_row(
            "SELECT COALESCE(SUM(length(transcription)),0) FROM main.sessions",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let bytes: i64 = tx
        .query_row(
            "SELECT COALESCE(SUM(length(CAST(transcription AS BLOB))),0) FROM main.sessions",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if chars != transcript_chars || bytes != transcript_bytes {
        return Err("迁移后的转写字符数或字节数与源数据不一致。".into());
    }
    Ok(())
}

/// Everything that runs on every launch after the schema is settled:
/// providers/settings/sentence_translations, provider seeding, and
/// connections table setup. No other code path creates tables.
fn finish_every_launch(db: &Connection) -> Result<(), String> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS providers(id TEXT PRIMARY KEY, name TEXT NOT NULL, base_url TEXT NOT NULL, models_json TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS settings(singleton INTEGER PRIMARY KEY CHECK(singleton=1), json TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS sentence_translations(source_text TEXT NOT NULL, target_language TEXT NOT NULL, translation TEXT NOT NULL, PRIMARY KEY(source_text, target_language));",
    )
    .map_err(|e| e.to_string())?;
    for (id, name, base) in [
        ("workbuddy", "WorkBuddy", "https://copilot.tencent.com/v2"),
        ("traecode", "TraeCode", "https://www.trae.ai"),
    ] {
        db.execute(
            "INSERT OR IGNORE INTO providers(id,name,base_url,models_json) VALUES(?,?,?,'[]')",
            rusqlite::params![id, name, base],
        )
        .map_err(|e| e.to_string())?;
    }
    crate::connections::initialize(db)
}

/// Runs the legacy-to-node-tree migration on an already-open connection whose
/// `user_version` is 0 and which has a `workspaces` table but no `nodes`
/// table. STEPS 0-6 of the spec's migration plan.
fn run_migration(mut db: Connection, path: &Path) -> Result<InitReport, InitError> {
    let legacy = introspect_legacy_columns(&db)?;
    let expected = preflight(&db, &legacy)?;
    let backup = create_verified_backup(&db, path, expected)?;

    db.execute("ATTACH DATABASE ?1 AS pre", [readonly_uri(&backup)])
        .map_err(|e| InitError::with_backup(format!("附加备份失败：{e}"), backup.clone()))?;

    let result = migrate_within_transaction(&mut db, &legacy, expected);
    let _ = db.execute("DETACH DATABASE pre", []);

    match result {
        Ok(()) => {
            db.execute_batch("PRAGMA foreign_keys=ON;")
                .map_err(|e| InitError::with_backup(e.to_string(), backup.clone()))?;
            let ok: String = db
                .query_row("PRAGMA integrity_check", [], |r| r.get(0))
                .map_err(|e| InitError::with_backup(e.to_string(), backup.clone()))?;
            if ok != "ok" {
                return Err(InitError::with_backup(
                    format!("迁移完成后完整性检查失败：{ok}"),
                    backup,
                ));
            }
            let post_fk: i64 = db
                .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                    r.get(0)
                })
                .map_err(|e| InitError::with_backup(e.to_string(), backup.clone()))?;
            if post_fk != 0 {
                return Err(InitError::with_backup(
                    "迁移完成后外键检查发现违例。",
                    backup,
                ));
            }
            eprintln!(
                "node tree migration: {} folders, {} sessions, {} materials, {} messages, transcript {} chars / {} bytes; backup: {}",
                expected.folders,
                expected.sessions,
                expected.materials,
                expected.messages,
                expected.transcript_chars,
                expected.transcript_bytes,
                backup.display(),
            );
            finish_every_launch(&db).map_err(|e| InitError::with_backup(e, backup.clone()))?;
            Ok(InitReport {
                migrated: Some(expected),
                backup: Some(backup),
            })
        }
        Err(message) => Err(InitError::with_backup(message, backup)),
    }
}

/// Entry point, called once from `AppState::open` before any other command
/// touches the database.
pub fn initialize_database(path: &Path) -> Result<InitReport, InitError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| InitError::simple(e.to_string()))?;
    }
    let db = Connection::open(path).map_err(|e| InitError::simple(e.to_string()))?;
    db.execute_batch("PRAGMA busy_timeout=5000; PRAGMA foreign_keys=OFF;")
        .map_err(|e| InitError::simple(e.to_string()))?;
    let fk_state: i64 = db
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .map_err(|e| InitError::simple(e.to_string()))?;
    if fk_state != 0 {
        return Err(InitError::simple("无法关闭外键约束，已停止启动。"));
    }
    let version: i64 = db
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| InitError::simple(e.to_string()))?;
    let tables = list_tables(&db)?;
    let has_workspaces = tables.iter().any(|t| t == "workspaces");
    let has_nodes = tables.iter().any(|t| t == "nodes");

    if version > SCHEMA_VERSION {
        return Err(InitError::simple(
            "数据库来自更新版本的 Async，已停止启动，数据未改动。",
        ));
    }
    if version == SCHEMA_VERSION {
        if has_workspaces {
            eprintln!(
                "node tree migration: ignoring residue `workspaces` table left by an older build"
            );
        }
        finish_every_launch(&db).map_err(InitError::simple)?;
        return Ok(InitReport {
            migrated: None,
            backup: None,
        });
    }
    // version == 0
    if !has_workspaces && !has_nodes {
        let mut db = db;
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| InitError::simple(e.to_string()))?;
        tx.execute_batch(SCHEMA_SQL)
            .map_err(|e| InitError::simple(e.to_string()))?;
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(|e| InitError::simple(e.to_string()))?;
        tx.commit().map_err(|e| InitError::simple(e.to_string()))?;
        finish_every_launch(&db).map_err(InitError::simple)?;
        return Ok(InitReport {
            migrated: None,
            backup: None,
        });
    }
    if has_workspaces && !has_nodes {
        return run_migration(db, path);
    }
    Err(InitError::simple(
        "数据库结构无法识别（user_version=0 但已存在 nodes 表），已停止启动，数据未改动。",
    ))
}

#[cfg(test)]
#[path = "../../test/tree_migration.rs"]
mod tree_migration_tests;

mod auth_commands;
mod connections;
mod credential_store;
mod models_catalog;
mod oauth;
mod recording;
mod stt;

use std::{
    collections::HashMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

use credential_store::Entry;
use quick_xml::{events::Event, Reader};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{ipc::Channel, Emitter, Manager};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use zip::ZipArchive;

const CONTEXT_LIMIT: usize = 48_000;
// Background notes trigger rule: while a session is recording, once the
// transcript has grown by this many new characters since the last covered
// point, a notes update runs; the chunk sent carries this many characters of
// overlap from before that point so context isn't lost at the boundary.
const NOTES_THRESHOLD_CHARS: usize = 800;
const NOTES_OVERLAP_CHARS: usize = 200;
// Live translation batching rule: several sentences that finish while recording
// often land within milliseconds of each other (a VAD flush, a long pause);
// this is how long a session+language's pending queue waits, once non-empty,
// before one request is sent for everything that accumulated - never one
// request per sentence.
const TRANSLATION_DEBOUNCE_MS: u64 = 1200;
// Backoff after a translation batch error: the normal debounce for the first
// retry, doubling per additional consecutive failure, capped so a persistent
// outage never waits longer than this between attempts.
const TRANSLATION_MAX_BACKOFF_MS: u64 = 30_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub id: String,
    pub name: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: String,
    pub role: String,
    pub content: String,
    // Empty for messages persisted before this field existed; never rewritten afterward.
    #[serde(default)]
    pub created_at: String,
    // Only present on an assistant turn that ran at least one local tool.
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCall>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    pub workspace_id: String,
    pub title: String,
    #[serde(default)]
    pub messages: Vec<Message>,
    pub transcription: Option<String>,
    pub summary: Option<String>,
    pub summary_updated_at: Option<String>,
    #[serde(default)]
    pub transcription_words: Option<Vec<stt::Word>>,
    // Gates the background notes job only; the explicit Generate/Refresh
    // button always works regardless of this flag.
    #[serde(default = "default_true")]
    pub notes_enabled: bool,
    // All three below gate/scope the background live-translation job only;
    // absent target_language means "follow the app's UI language".
    #[serde(default)]
    pub translation_enabled: bool,
    #[serde(default)]
    pub translation_target_language: Option<String>,
    #[serde(default = "default_translation_mode")]
    pub translation_mode: String,
}
fn default_true() -> bool {
    true
}
fn default_translation_mode() -> String {
    "side-by-side".into()
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Material {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub content: String,
    pub path: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub provider_id: String,
    pub model: String,
    pub theme: String,
    pub language: String,
    #[serde(default = "default_main_panel_ratio")]
    pub main_panel_ratio: f64,
    #[serde(default = "default_sidebar_open")]
    pub sidebar_open: bool,
}
fn default_main_panel_ratio() -> f64 {
    0.62
}
fn default_sidebar_open() -> bool {
    true
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            provider_id: "openai".into(),
            model: String::new(),
            theme: "system".into(),
            language: "zh".into(),
            main_panel_ratio: default_main_panel_ratio(),
            sidebar_open: default_sidebar_open(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    #[serde(default)]
    pub auth_method: String,
    pub id: String,
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub models: Vec<String>,
    pub has_key: bool,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppData {
    pub workspaces: Vec<Workspace>,
    pub sessions: Vec<Session>,
    pub materials: Vec<Material>,
    pub settings: Settings,
    pub providers: Vec<Provider>,
}
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamEvent {
    #[serde(rename = "type")]
    pub kind: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_arguments: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_result: Option<String>,
}
impl StreamEvent {
    pub fn delta(text: String) -> Self {
        Self {
            kind: "delta".into(),
            text,
            ..Self::default()
        }
    }
    pub fn done() -> Self {
        Self {
            kind: "done".into(),
            ..Self::default()
        }
    }
}
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum RecordingEvent {
    #[serde(rename_all = "camelCase")]
    Transcript { session_id: String, text: String },
    #[serde(rename_all = "camelCase")]
    Error { session_id: String, text: String },
    #[serde(rename_all = "camelCase")]
    Level { session_id: String, level: f32 },
}

pub struct AppState {
    db_path: PathBuf,
    stt: stt::SttManager,
    recording: recording::RecordingManager,
    client: reqwest::Client,
    cancellations: Mutex<HashMap<String, CancellationToken>>,
    // Separate from `cancellations` so a background notes job can never
    // reserve the same slot chat needs and block the user from chatting.
    notes_cancellations: Mutex<HashMap<String, CancellationToken>>,
    notes: Mutex<HashMap<String, NotesTracker>>,
    // A third, independent map again: translation writes its own cache table
    // rather than the `summary` column notes/chat share, so it never needs to
    // defer to either of them and must never share a cancellation slot with them.
    translation_cancellations: Mutex<HashMap<String, CancellationToken>>,
    translations: Mutex<HashMap<String, TranslationTracker>>,
}

#[derive(Default)]
struct NotesTracker {
    // Transcript characters already folded into the notes.
    cursor: usize,
    running: bool,
    // Set when the transcript crosses the threshold again while a job for
    // this session is already running (or chat/explicit-summarize is using
    // the session); picked up as one more round once that clears, so jobs
    // never stack for the same session.
    pending: bool,
    // Set synchronously by stop_recording under the same lock
    // on_transcript_appended checks before starting a round, so a transcript
    // append racing the stop sweep can never start (or spend tokens on) a job
    // for a session that has already stopped. Cleared by the next
    // start_recording for this session.
    stopped: bool,
}
// Keyed by `translation_key(session_id, target_language)`: one batch queue per
// session+language pair, so switching languages mid-recording starts a fresh
// queue instead of mixing pending sentences meant for two different languages.
#[derive(Default)]
struct TranslationTracker {
    // Source sentence texts awaiting translation, insertion order, deduped.
    pending: Vec<String>,
    // True from the moment a debounce flush is scheduled until that flush's
    // loop finds the pending queue empty; guards against spawning a second
    // debounce timer while one is already outstanding or running.
    scheduled: bool,
    // Consecutive request failures for this session+language, reset to 0 on
    // the next success; drives the backoff delay between retries so a
    // persistent provider failure can't turn the debounce loop into a tight
    // hammering loop.
    consecutive_failures: u32,
}

impl AppState {
    pub fn open(db_path: PathBuf) -> Result<Self, String> {
        let stt_root = db_path
            .parent()
            .ok_or("本地数据路径无效。")?
            .join("voice-models");
        let state = Self {
            stt: stt::SttManager::new(stt_root),
            recording: recording::RecordingManager::new(),
            db_path,
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(120))
                .build()
                .map_err(|e| e.to_string())?,
            cancellations: Mutex::new(HashMap::new()),
            notes_cancellations: Mutex::new(HashMap::new()),
            notes: Mutex::new(HashMap::new()),
            translation_cancellations: Mutex::new(HashMap::new()),
            translations: Mutex::new(HashMap::new()),
        };
        state.db().map(|_| state)
    }
    pub fn database_path(&self) -> &Path {
        &self.db_path
    }
    fn db(&self) -> Result<Connection, String> {
        open_database(&self.db_path)
    }
    fn key(&self, id: &str) -> Result<Entry, String> {
        Entry::new(
            self.db_path
                .parent()
                .ok_or("本地数据路径无效。")?
                .join("credentials"),
            id,
        )
        .map_err(|e| format!("本地凭据文件不可用：{e}"))
    }
}

fn open_database(path: &Path) -> Result<Connection, String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let db = Connection::open(path).map_err(|e| e.to_string())?;
    db.execute_batch("PRAGMA foreign_keys = ON;
      CREATE TABLE IF NOT EXISTS workspaces(id TEXT PRIMARY KEY, name TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS sessions(id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE, title TEXT NOT NULL, transcription TEXT, summary TEXT, summary_updated_at TEXT);
      CREATE TABLE IF NOT EXISTS messages(id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, role TEXT NOT NULL, content TEXT NOT NULL, position INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS materials(id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE, name TEXT NOT NULL, content TEXT NOT NULL, path TEXT);
      CREATE TABLE IF NOT EXISTS providers(id TEXT PRIMARY KEY, name TEXT NOT NULL, base_url TEXT NOT NULL, models_json TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS settings(singleton INTEGER PRIMARY KEY CHECK(singleton=1), json TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS sentence_translations(source_text TEXT NOT NULL, target_language TEXT NOT NULL, translation TEXT NOT NULL, PRIMARY KEY(source_text, target_language));") .map_err(|e| e.to_string())?;
    // Additive migrations for columns that postdate a user's existing database;
    // guarded so they only ever run once and never touch existing rows.
    ensure_column(&db, "sessions", "summary_updated_at", "TEXT")?;
    ensure_column(&db, "sessions", "transcription_words", "TEXT")?;
    ensure_column(
        &db,
        "sessions",
        "notes_enabled",
        "INTEGER NOT NULL DEFAULT 1",
    )?;
    ensure_column(
        &db,
        "sessions",
        "translation_enabled",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(&db, "sessions", "translation_target_language", "TEXT")?;
    ensure_column(
        &db,
        "sessions",
        "translation_mode",
        "TEXT NOT NULL DEFAULT 'side-by-side'",
    )?;
    ensure_column(&db, "messages", "created_at", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(&db, "messages", "tool_calls", "TEXT")?;
    for (id, name, base) in [
        ("workbuddy", "WorkBuddy", "https://copilot.tencent.com/v2"),
        ("traecode", "TraeCode", "https://www.trae.ai"),
    ] {
        db.execute(
            "INSERT OR IGNORE INTO providers(id,name,base_url,models_json) VALUES(?,?,?,'[]')",
            params![id, name, base],
        )
        .map_err(|e| e.to_string())?;
    }
    connections::initialize(&db)?;
    Ok(db)
}

fn ensure_column(
    db: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), String> {
    let exists: i64 = db
        .query_row(
            &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name=?"),
            [column],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    if exists == 0 {
        db.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
            [],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn id() -> String {
    Uuid::new_v4().to_string()
}
fn limit(value: String) -> String {
    if value.len() <= CONTEXT_LIMIT {
        return value;
    }
    value[..value.floor_char_boundary(CONTEXT_LIMIT)].to_string()
}
fn row_workspace(row: &rusqlite::Row<'_>) -> rusqlite::Result<Workspace> {
    Ok(Workspace {
        id: row.get(0)?,
        name: row.get(1)?,
    })
}
fn row_material(row: &rusqlite::Row<'_>) -> rusqlite::Result<Material> {
    Ok(Material {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        name: row.get(2)?,
        content: row.get(3)?,
        path: row.get(4)?,
    })
}

fn load_data(state: &AppState) -> Result<AppData, String> {
    let db = state.db()?;
    let workspaces = db
        .prepare("SELECT id,name FROM workspaces ORDER BY rowid")
        .map_err(|e| e.to_string())?
        .query_map([], row_workspace)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let mut sessions = Vec::new();
    let mut statement = db
        .prepare(
            "SELECT id,workspace_id,title,transcription,summary,summary_updated_at,transcription_words,notes_enabled,translation_enabled,translation_target_language,translation_mode FROM sessions ORDER BY rowid",
        )
        .map_err(|e| e.to_string())?;
    let rows = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, bool>(7)?,
                r.get::<_, bool>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, String>(10)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    for row in rows {
        let (
            sid,
            workspace_id,
            title,
            transcription,
            summary,
            summary_updated_at,
            transcription_words,
            notes_enabled,
            translation_enabled,
            translation_target_language,
            translation_mode,
        ) = row.map_err(|e| e.to_string())?;
        let transcription_words = transcription_words
            .map(|raw| serde_json::from_str(&raw).map_err(|e| e.to_string()))
            .transpose()?;
        let messages = db
            .prepare(
                "SELECT id,role,content,created_at,tool_calls FROM messages WHERE session_id=? ORDER BY position",
            )
            .map_err(|e| e.to_string())?
            .query_map([&sid], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|(id, role, content, created_at, tool_calls)| {
                let tool_calls = tool_calls
                    .map(|raw| serde_json::from_str(&raw).map_err(|e| e.to_string()))
                    .transpose()?;
                Ok(Message {
                    id,
                    role,
                    content,
                    created_at,
                    tool_calls,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        sessions.push(Session {
            id: sid,
            workspace_id,
            title,
            messages,
            transcription,
            summary,
            summary_updated_at,
            transcription_words,
            notes_enabled,
            translation_enabled,
            translation_target_language,
            translation_mode,
        });
    }
    let materials = db
        .prepare("SELECT id,workspace_id,name,content,path FROM materials ORDER BY rowid")
        .map_err(|e| e.to_string())?
        .query_map([], row_material)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let settings = db
        .query_row("SELECT json FROM settings WHERE singleton=1", [], |r| {
            r.get::<_, String>(0)
        })
        .optional()
        .map_err(|e| e.to_string())?
        .map(|raw| serde_json::from_str(&raw).map_err(|e| e.to_string()))
        .transpose()?
        .unwrap_or_default();
    let mut providers = Vec::new();
    let mut provider_statement = db
        .prepare("SELECT id,name,base_url,models_json FROM providers ORDER BY rowid")
        .map_err(|e| e.to_string())?;
    let entries = provider_statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    for entry in entries {
        let (id, name, base_url, models) = entry.map_err(|e| e.to_string())?;
        let models =
            serde_json::from_str(&models).map_err(|_| format!("提供商 {id} 的模型数据已损坏。"))?;
        providers.push(Provider {
            auth_method: if oauth::is_oauth(&id) {
                "oauth"
            } else {
                "api-key"
            }
            .into(),
            has_key: connections::has_credential_db(&db, &id)?,
            id,
            name,
            base_url,
            models,
        });
    }
    Ok(AppData {
        workspaces,
        sessions,
        materials,
        settings,
        providers,
    })
}

#[tauri::command]
fn load_state(state: tauri::State<'_, AppState>) -> Result<AppData, String> {
    load_data(&state)
}
#[tauri::command]
fn create_workspace(name: String, state: tauri::State<'_, AppState>) -> Result<Workspace, String> {
    let item = Workspace { id: id(), name };
    state
        .db()?
        .execute(
            "INSERT INTO workspaces(id,name) VALUES(?,?)",
            params![item.id, item.name],
        )
        .map_err(|e| e.to_string())?;
    Ok(item)
}
fn rename_workspace_impl(state: &AppState, id: &str, name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("名称不能为空。".into());
    }
    let updated = state
        .db()?
        .execute(
            "UPDATE workspaces SET name=? WHERE id=?",
            params![name, id],
        )
        .map_err(|e| e.to_string())?;
    if updated == 0 {
        return Err("找不到工作区。".into());
    }
    Ok(())
}
#[tauri::command]
fn rename_workspace(
    id: String,
    name: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    rename_workspace_impl(&state, &id, &name)
}
#[tauri::command]
fn delete_workspace(id: String, state: tauri::State<'_, AppState>) -> Result<(), String> {
    state
        .db()?
        .execute("DELETE FROM workspaces WHERE id=?", [id])
        .map_err(|e| e.to_string())?;
    Ok(())
}
#[tauri::command]
fn create_session(
    workspace_id: String,
    title: String,
    state: tauri::State<'_, AppState>,
) -> Result<Session, String> {
    let item = Session {
        id: id(),
        workspace_id,
        title,
        messages: vec![],
        transcription: None,
        summary: None,
        summary_updated_at: None,
        transcription_words: None,
        notes_enabled: true,
        translation_enabled: false,
        translation_target_language: None,
        translation_mode: default_translation_mode(),
    };
    state
        .db()?
        .execute(
            "INSERT INTO sessions(id,workspace_id,title) VALUES(?,?,?)",
            params![item.id, item.workspace_id, item.title],
        )
        .map_err(|e| e.to_string())?;
    Ok(item)
}
fn rename_session_impl(state: &AppState, id: &str, title: &str) -> Result<(), String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("名称不能为空。".into());
    }
    let updated = state
        .db()?
        .execute("UPDATE sessions SET title=? WHERE id=?", params![title, id])
        .map_err(|e| e.to_string())?;
    if updated == 0 {
        return Err("找不到会话。".into());
    }
    Ok(())
}
#[tauri::command]
fn rename_session(
    id: String,
    title: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    rename_session_impl(&state, &id, &title)
}
fn set_notes_enabled_impl(state: &AppState, id: &str, enabled: bool) -> Result<(), String> {
    let updated = state
        .db()?
        .execute(
            "UPDATE sessions SET notes_enabled=? WHERE id=?",
            params![enabled, id],
        )
        .map_err(|e| e.to_string())?;
    if updated == 0 {
        return Err("找不到会话。".into());
    }
    if !enabled {
        stop_notes_for_session(state, id);
    }
    Ok(())
}
#[tauri::command]
fn set_notes_enabled(
    id: String,
    enabled: bool,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    set_notes_enabled_impl(&state, &id, enabled)
}
fn set_translation_settings_impl(
    state: &AppState,
    id: &str,
    enabled: bool,
    target_language: Option<&str>,
    mode: &str,
) -> Result<(), String> {
    if !matches!(mode, "side-by-side" | "separate") {
        return Err("无效的显示方式。".into());
    }
    let updated = state
        .db()?
        .execute(
            "UPDATE sessions SET translation_enabled=?,translation_target_language=?,translation_mode=? WHERE id=?",
            params![enabled, target_language, mode, id],
        )
        .map_err(|e| e.to_string())?;
    if updated == 0 {
        return Err("找不到会话。".into());
    }
    if !enabled {
        stop_translation_for_session(state, id);
    }
    Ok(())
}
#[tauri::command]
fn set_translation_settings(
    id: String,
    enabled: bool,
    target_language: Option<String>,
    mode: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    set_translation_settings_impl(&state, &id, enabled, target_language.as_deref(), &mode)
}
#[tauri::command]
fn delete_session(id: String, state: tauri::State<'_, AppState>) -> Result<(), String> {
    stop_notes_for_session(&state, &id);
    stop_translation_for_session(&state, &id);
    state
        .db()?
        .execute("DELETE FROM sessions WHERE id=?", [id])
        .map_err(|e| e.to_string())?;
    Ok(())
}
#[tauri::command]
fn save_session(session: Session, state: tauri::State<'_, AppState>) -> Result<(), String> {
    save_session_with(session, &state)
}
fn save_session_with(session: Session, state: &AppState) -> Result<(), String> {
    let mut db = state.db()?;
    let tx = db.transaction().map_err(|e| e.to_string())?;
    let updated = tx
        .execute(
            "UPDATE sessions SET title=?,transcription=?,summary=?,summary_updated_at=?,notes_enabled=?,translation_enabled=?,translation_target_language=?,translation_mode=? WHERE id=?",
            params![
                session.title,
                session.transcription,
                session.summary,
                session.summary_updated_at,
                session.notes_enabled,
                session.translation_enabled,
                session.translation_target_language,
                session.translation_mode,
                session.id
            ],
        )
        .map_err(|e| e.to_string())?;
    if updated == 0 {
        return Err("找不到会话。".into());
    }
    tx.execute("DELETE FROM messages WHERE session_id=?", [&session.id])
        .map_err(|e| e.to_string())?;
    for (position, m) in session.messages.iter().enumerate() {
        // Reinserted on every save, so a blank created_at (edit/regenerate of an
        // existing message) keeps its original value instead of being reset here;
        // only a genuinely new message without one gets stamped now.
        let created_at = if m.created_at.trim().is_empty() {
            chrono::Utc::now().to_rfc3339()
        } else {
            m.created_at.clone()
        };
        let tool_calls = m
            .tool_calls
            .as_ref()
            .filter(|calls| !calls.is_empty())
            .map(|calls| serde_json::to_string(calls).map_err(|e| e.to_string()))
            .transpose()?;
        tx.execute(
            "INSERT INTO messages(id,session_id,role,content,position,created_at,tool_calls) VALUES(?,?,?,?,?,?,?)",
            params![m.id, session.id, m.role, m.content, position, created_at, tool_calls],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())
}
#[tauri::command]
fn save_settings(settings: Settings, state: tauri::State<'_, AppState>) -> Result<(), String> {
    let json = serde_json::to_string(&settings).map_err(|e| e.to_string())?;
    state.db()?.execute("INSERT INTO settings(singleton,json) VALUES(1,?) ON CONFLICT(singleton) DO UPDATE SET json=excluded.json",[json]).map_err(|e|e.to_string())?;
    Ok(())
}
#[tauri::command]
fn save_material(material: Material, state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.db()?.execute("INSERT INTO materials(id,workspace_id,name,content,path) VALUES(?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,content=excluded.content,path=excluded.path",params![material.id,material.workspace_id,material.name,material.content,material.path]).map_err(|e|e.to_string())?;
    Ok(())
}
#[tauri::command]
fn delete_material(id: String, state: tauri::State<'_, AppState>) -> Result<(), String> {
    state
        .db()?
        .execute("DELETE FROM materials WHERE id=?", [id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn extract_docx(path: &Path) -> Result<String, String> {
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut zip = ZipArchive::new(file).map_err(|e| format!("无效 DOCX：{e}"))?;
    let mut xml = String::new();
    zip.by_name("word/document.xml")
        .map_err(|_| "DOCX 不包含正文。".to_string())?
        .read_to_string(&mut xml)
        .map_err(|e| e.to_string())?;
    let mut reader = Reader::from_str(&xml);
    let mut output = String::new();
    let mut paragraph = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.name().as_ref() == b"w:p" => {
                if paragraph && !output.ends_with('\n') {
                    output.push('\n')
                };
                paragraph = true;
            }
            Ok(Event::Text(t)) => output.push_str(&t.unescape().map_err(|e| e.to_string())?),
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("无法读取 DOCX：{e}")),
            _ => {}
        }
    }
    Ok(output)
}
fn read_material(path: &Path) -> Result<String, String> {
    if fs::metadata(path).map_err(|e| e.to_string())?.len() > 8 * 1024 * 1024 {
        return Err("材料不能超过 8 MiB。".into());
    }
    match path
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "txt" | "md" | "markdown" => fs::read_to_string(path).map_err(|e| e.to_string()),
        "docx" => extract_docx(path),
        _ => Err("仅支持 TXT、Markdown 和 DOCX 材料。".into()),
    }
}
#[tauri::command]
fn import_material(
    workspace_id: String,
    path: String,
    state: tauri::State<'_, AppState>,
) -> Result<Material, String> {
    let path_buf = PathBuf::from(&path);
    let name = path_buf
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("无效文件名。")?
        .to_string();
    let item = Material {
        id: id(),
        workspace_id,
        name,
        content: read_material(&path_buf)?,
        path: Some(path),
    };
    save_material(item.clone(), state)?;
    Ok(item)
}

#[tauri::command]
fn delete_provider(id: String, state: tauri::State<'_, AppState>) -> Result<(), String> {
    if id == "openai" || oauth::is_oauth(&id) {
        return Err("内置供应商可移除授权，但不能删除。".into());
    }
    for credential in connections::list(&state, &id)? {
        connections::remove(&state, &id, &credential.id, &credential.auth_method)?;
    }
    state
        .db()?
        .execute("DELETE FROM providers WHERE id=?", [&id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn endpoint(base: &str, suffix: &str) -> String {
    format!("{}/{suffix}", base.trim_end_matches('/'))
}

fn provider(state: &AppState, provider_id: &str) -> Result<Provider, String> {
    load_data(state)?
        .providers
        .into_iter()
        .find(|p| p.id == provider_id)
        .ok_or_else(|| "找不到提供商。".into())
}
#[tauri::command]
async fn discover_models(
    provider_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<String>, String> {
    auth_commands::models(&state, &provider_id).await
}

fn session_context(
    state: &AppState,
    session_id: &str,
) -> Result<(Session, Vec<Material>, Settings), String> {
    let all = load_data(state)?;
    let session = all
        .sessions
        .into_iter()
        .find(|s| s.id == session_id)
        .ok_or("找不到会话。")?;
    let materials = all
        .materials
        .into_iter()
        .filter(|m| m.workspace_id == session.workspace_id)
        .collect();
    Ok((session, materials, all.settings))
}
fn persist_message(
    state: &AppState,
    session_id: &str,
    role: &str,
    content: &str,
    tool_calls: Option<&[ToolCall]>,
) -> Result<(), String> {
    let tool_calls_json = tool_calls
        .filter(|calls| !calls.is_empty())
        .map(|calls| serde_json::to_string(calls).map_err(|e| e.to_string()))
        .transpose()?;
    let mut db = state.db()?;
    let tx = db.transaction().map_err(|e| e.to_string())?;
    let position: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(position),-1)+1 FROM messages WHERE session_id=?",
            [session_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO messages(id,session_id,role,content,position,created_at,tool_calls) VALUES(?,?,?,?,?,?,?)",
        params![
            id(),
            session_id,
            role,
            content,
            position,
            chrono::Utc::now().to_rfc3339(),
            tool_calls_json
        ],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}
fn reserve_token(
    map: &Mutex<HashMap<String, CancellationToken>>,
    key: &str,
    busy_message: &str,
) -> Result<CancellationToken, String> {
    let mut active = map.lock().map_err(|_| "生成状态不可用。")?;
    if active.contains_key(key) {
        return Err(busy_message.into());
    }
    let token = CancellationToken::new();
    active.insert(key.into(), token.clone());
    Ok(token)
}
fn generation_token(state: &AppState, session_id: &str) -> Result<CancellationToken, String> {
    reserve_token(&state.cancellations, session_id, "该会话已有生成任务。")
}
// Its own map, distinct from `cancellations`: a background notes job must
// never reserve the same slot chat needs, or it would block the user from
// chatting in the session they are recording.
fn notes_generation_token(state: &AppState, session_id: &str) -> Result<CancellationToken, String> {
    reserve_token(
        &state.notes_cancellations,
        session_id,
        "该会话已有笔记生成任务。",
    )
}
fn stop_notes_for_session(state: &AppState, session_id: &str) {
    if let Ok(mut jobs) = state.notes.lock() {
        // Marked under this same lock so a transcript append racing this
        // sweep either loses the race entirely (sees `stopped` and refuses to
        // start) or already started strictly before this sweep ran.
        let entry = jobs.entry(session_id.to_string()).or_default();
        entry.running = false;
        entry.pending = false;
        entry.stopped = true;
    }
    if let Ok(mut tokens) = state.notes_cancellations.lock() {
        if let Some(token) = tokens.remove(session_id) {
            token.cancel();
        }
    }
}
// One session can have pending/in-flight translation jobs for several target
// languages at once (each keyed by translation_key), so this sweeps every key
// carrying this session's prefix rather than a single exact key.
fn stop_translation_for_session(state: &AppState, session_id: &str) {
    let prefix = format!("{session_id}\u{0}");
    if let Ok(mut jobs) = state.translations.lock() {
        jobs.retain(|key, _| !key.starts_with(&prefix));
    }
    if let Ok(mut tokens) = state.translation_cancellations.lock() {
        let keys: Vec<String> = tokens
            .keys()
            .filter(|key| key.starts_with(&prefix))
            .cloned()
            .collect();
        for key in keys {
            if let Some(token) = tokens.remove(&key) {
                token.cancel();
            }
        }
    }
}
async fn streamed_completion(
    state: &AppState,
    session_id: &str,
    messages: Vec<Value>,
    tool_workspace_id: Option<&str>,
    channel: Channel<StreamEvent>,
    cancellation: CancellationToken,
) -> Result<(String, Vec<ToolCall>), String> {
    let (_, _, settings) = session_context(state, session_id)?;
    auth_commands::stream(
        state,
        &settings.provider_id,
        &settings.model,
        messages,
        tool_workspace_id,
        cancellation,
        channel,
    )
    .await
}
async fn chat_impl(
    state: &AppState,
    session_id: &str,
    content: &str,
    on_event: Channel<StreamEvent>,
) -> Result<(), String> {
    let (session, materials, _) = session_context(state, session_id)?;
    let workspace_id = session.workspace_id.clone();
    // Reserve the generation slot before persisting anything, so a rejected
    // concurrent call never leaves a reply-less user message behind.
    let token = generation_token(state, session_id)?;
    let mut context = String::new();
    if let Some(text) = session.transcription {
        context.push_str("Transcript:\n");
        context.push_str(&text);
        context.push('\n');
    }
    for material in materials {
        context.push_str("Material ");
        context.push_str(&material.name);
        context.push_str(":\n");
        context.push_str(&material.content);
        context.push('\n');
        if context.len() >= CONTEXT_LIMIT {
            break;
        }
    }
    let mut messages = Vec::new();
    // Lets the frontend highlight uncertain spans and offer a verify-this-claim follow-up.
    messages.push(json!({"role":"system","content":"When a factual statement is uncertain, unverified, estimated, or likely stale, wrap only that span with <maybe> and </maybe>. Do not wrap entire answers unless everything is uncertain, and do not explain this rule unless asked."}));
    if !context.is_empty() {
        messages.push(json!({"role":"system","content":limit(context)}));
    }
    messages.extend(
        session
            .messages
            .into_iter()
            .map(|m| json!({"role":m.role,"content":m.content})),
    );
    messages.push(json!({"role":"user","content":content}));
    persist_message(state, session_id, "user", content, None)?;
    let result = streamed_completion(
        state,
        session_id,
        messages,
        Some(&workspace_id),
        on_event,
        token,
    )
    .await;
    state
        .cancellations
        .lock()
        .map_err(|_| "生成状态不可用。")?
        .remove(session_id);
    let (answer, tool_calls) = result?;
    if !answer.is_empty() || !tool_calls.is_empty() {
        persist_message(state, session_id, "assistant", &answer, Some(&tool_calls))?;
    }
    Ok(())
}
#[tauri::command]
async fn chat(
    session_id: String,
    content: String,
    on_event: Channel<StreamEvent>,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    chat_impl(&state, &session_id, &content, on_event).await
}
#[tauri::command]
fn cancel_generation(session_id: String, state: tauri::State<'_, AppState>) -> Result<(), String> {
    if let Some(token) = state
        .cancellations
        .lock()
        .map_err(|_| "生成状态不可用。")?
        .get(&session_id)
    {
        token.cancel()
    }
    Ok(())
}
#[tauri::command]
async fn summarize(
    session_id: String,
    on_event: Channel<StreamEvent>,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    // The explicit button always wins: cancel any in-flight or pending
    // background notes round first, since both write the same summary
    // column and must never race each other.
    stop_notes_for_session(&state, &session_id);
    let (session, materials, _) = session_context(&state, &session_id)?;
    let mut source = session
        .messages
        .iter()
        .map(|m| format!("{}: {}", m.role, m.content))
        .collect::<Vec<_>>()
        .join("\n");
    if let Some(transcript) = session.transcription.as_deref() {
        source.push_str("\nTranscript:\n");
        source.push_str(transcript);
    }
    for material in materials {
        source.push_str("\nMaterial ");
        source.push_str(&material.name);
        source.push_str(":\n");
        source.push_str(&material.content);
    }
    let messages = vec![
        json!({"role":"system","content":"Summarize the following class session clearly and concisely."}),
        json!({"role":"user","content":limit(source)}),
    ];
    let token = generation_token(&state, &session_id)?;
    let result = streamed_completion(&state, &session_id, messages, None, on_event, token).await;
    state
        .cancellations
        .lock()
        .map_err(|_| "生成状态不可用。")?
        .remove(&session_id);
    let (summary, _) = result?;
    persist_notes(&state, &session_id, &summary)
}
fn persist_notes(state: &AppState, session_id: &str, notes: &str) -> Result<(), String> {
    let updated_at = chrono::Utc::now().to_rfc3339();
    state
        .db()?
        .execute(
            "UPDATE sessions SET summary=?,summary_updated_at=? WHERE id=?",
            params![notes, updated_at, session_id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// True once at least one enabled, healthy credential can serve the
/// configured provider/model; lets the background notes job skip a doomed
/// model call instead of erroring on every threshold crossing.
fn provider_ready(state: &AppState, settings: &Settings) -> bool {
    if settings.model.trim().is_empty() {
        return false;
    }
    connections::candidates(state, &settings.provider_id, &settings.model)
        .map(|items| !items.is_empty())
        .unwrap_or(false)
}
/// Pure trigger rule shared by production and tests: once the transcript has
/// grown by NOTES_THRESHOLD_CHARS past `cursor`, returns the chunk to send
/// (carrying NOTES_OVERLAP_CHARS of context from before the boundary) plus the
/// new cursor, or None when there isn't enough new transcript yet.
fn next_notes_chunk(transcript: &str, cursor: usize) -> Option<(String, usize)> {
    let len = transcript.len();
    if len <= cursor || len - cursor < NOTES_THRESHOLD_CHARS {
        return None;
    }
    let start = transcript.floor_char_boundary(cursor.saturating_sub(NOTES_OVERLAP_CHARS));
    let new_cursor = transcript.floor_char_boundary(len.saturating_sub(NOTES_OVERLAP_CHARS));
    Some((transcript[start..].to_string(), new_cursor))
}
async fn generate_notes_impl(
    state: &AppState,
    session_id: &str,
    chunk: &str,
    on_event: Channel<StreamEvent>,
) -> Result<(), String> {
    let (session, _, _) = session_context(state, session_id)?;
    let token = notes_generation_token(state, session_id)?;
    let mut user_content = String::new();
    if let Some(existing) = session.summary.as_deref().filter(|s| !s.trim().is_empty()) {
        user_content.push_str("Notes so far:\n");
        user_content.push_str(existing);
        user_content.push_str("\n\n");
    }
    user_content.push_str("New transcript excerpt since the last update:\n");
    user_content.push_str(&limit(chunk.to_owned()));
    let messages = vec![
        json!({"role":"system","content":"You maintain structured Markdown lecture notes for a class transcript that is still being recorded live. You are given the notes written so far (if any) and a new transcript excerpt. Reply with the complete, updated notes in well-structured Markdown, folding the new excerpt in rather than just appending it, and without mentioning that this is an update."}),
        json!({"role":"user","content":user_content}),
    ];
    let result = streamed_completion(state, session_id, messages, None, on_event, token).await;
    state
        .notes_cancellations
        .lock()
        .map_err(|_| "笔记生成状态不可用。")?
        .remove(session_id);
    let (notes, _) = result?;
    persist_notes(state, session_id, &notes)
}
/// Fires once per notes round so the caller can push a "notes-status" event
/// to the frontend without polling the database.
enum NotesEvent {
    NeedsProvider,
    Started,
    Finished {
        summary: Option<String>,
        summary_updated_at: Option<String>,
        error: Option<String>,
    },
}
/// Drives the background notes job for one session after new transcript has
/// been appended: coalesces (a job already running, or chat/explicit-summarize
/// holding the generation slot, just marks growth pending for the next append
/// to retry) and never reserves `cancellations` (chat's own map), so at most
/// one notes request is ever in flight and chat is never blocked by it.
async fn on_transcript_appended(
    state: &AppState,
    session_id: &str,
    on_event: &(impl Fn(NotesEvent) + Send + Sync),
) {
    loop {
        let (session, _, settings) = match session_context(state, session_id) {
            Ok(value) => value,
            Err(_) => return,
        };
        if !session.notes_enabled {
            return;
        }
        if !provider_ready(state, &settings) {
            on_event(NotesEvent::NeedsProvider);
            return;
        }
        let transcript = session.transcription.unwrap_or_default();
        let chunk = {
            let Ok(mut jobs) = state.notes.lock() else {
                return;
            };
            let entry = jobs.entry(session_id.to_string()).or_default();
            // Closes the stop_recording race: whichever of the two writers
            // reaches this lock first wins, and a stopped session can never
            // flip back to running without a fresh start_recording clearing
            // this flag first.
            if entry.stopped {
                return;
            }
            if entry.running {
                entry.pending = true;
                return;
            }
            match next_notes_chunk(&transcript, entry.cursor) {
                Some((chunk, new_cursor)) => {
                    // Defer to chat/explicit-summarize instead of racing them
                    // over the shared summary column; retried on the next
                    // transcript append.
                    let busy = state
                        .cancellations
                        .lock()
                        .map(|active| active.contains_key(session_id))
                        .unwrap_or(false);
                    if busy {
                        entry.pending = true;
                        return;
                    }
                    entry.running = true;
                    entry.pending = false;
                    entry.cursor = new_cursor;
                    chunk
                }
                None => return,
            }
        };
        on_event(NotesEvent::Started);
        let channel = Channel::<StreamEvent>::new(|_| Ok(()));
        let result = generate_notes_impl(state, session_id, &chunk, channel).await;
        let finished = match result {
            Ok(()) => {
                let (session, _, _) = match session_context(state, session_id) {
                    Ok(value) => value,
                    // Session was deleted while the job ran; nothing left to report.
                    Err(_) => return,
                };
                NotesEvent::Finished {
                    summary: session.summary,
                    summary_updated_at: session.summary_updated_at,
                    error: None,
                }
            }
            Err(error) => NotesEvent::Finished {
                summary: None,
                summary_updated_at: None,
                error: Some(error),
            },
        };
        on_event(finished);
        let rerun = {
            let Ok(mut jobs) = state.notes.lock() else {
                return;
            };
            match jobs.get_mut(session_id) {
                // Never happens in practice (stop marks `stopped` rather than
                // removing the entry) but handled defensively.
                None => false,
                Some(entry) => {
                    entry.running = false;
                    // Stopped while this round was in flight: never start
                    // another for it, regardless of pending.
                    if entry.stopped {
                        entry.pending = false;
                        false
                    } else {
                        std::mem::take(&mut entry.pending)
                    }
                }
            }
        };
        if !rerun {
            return;
        }
    }
}
/// Turns one `NotesEvent` into a "notes-status" event the frontend listens
/// for, so it can update the summary panel without polling.
fn emit_notes_event(app: &tauri::AppHandle, session_id: &str, event: NotesEvent) {
    let payload = match event {
        NotesEvent::NeedsProvider => json!({"sessionId": session_id, "status": "needs-provider"}),
        NotesEvent::Started => json!({"sessionId": session_id, "status": "generating"}),
        NotesEvent::Finished {
            summary,
            summary_updated_at,
            error,
        } => json!({
            "sessionId": session_id,
            "status": if error.is_some() { "error" } else { "idle" },
            "summary": summary,
            "summaryUpdatedAt": summary_updated_at,
        }),
    };
    let _ = app.emit("notes-status", payload);
}

// One pending/cancellation queue per session+language pair, so switching the
// target language mid-recording starts a fresh queue rather than mixing
// sentences meant for two different languages.
fn translation_key(session_id: &str, target_language: &str) -> String {
    format!("{session_id}\u{0}{target_language}")
}
/// Looks up each sentence in the persisted cache; anything not found is added
/// to that session+language's pending batch (deduped) for the debounce flush
/// to pick up. Returns the immediate cache hits (the caller can show these
/// right away) and whether a new flush needs to be scheduled.
fn enqueue_translations(
    state: &AppState,
    session_id: &str,
    target_language: &str,
    sentences: &[String],
) -> Result<(HashMap<String, String>, bool), String> {
    let db = state.db()?;
    let mut hits = HashMap::new();
    let mut misses = Vec::new();
    for sentence in sentences {
        let text = sentence.trim();
        if text.is_empty() {
            continue;
        }
        let cached: Option<String> = db
            .query_row(
                "SELECT translation FROM sentence_translations WHERE source_text=? AND target_language=?",
                params![text, target_language],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        match cached {
            Some(translation) => {
                hits.insert(text.to_string(), translation);
            }
            None => misses.push(text.to_string()),
        }
    }
    if misses.is_empty() {
        return Ok((hits, false));
    }
    let key = translation_key(session_id, target_language);
    let mut jobs = state.translations.lock().map_err(|_| "翻译状态不可用。")?;
    let entry = jobs.entry(key).or_default();
    for text in misses {
        if !entry.pending.iter().any(|existing| existing == &text) {
            entry.pending.push(text);
        }
    }
    let should_schedule = !entry.scheduled;
    entry.scheduled = true;
    Ok((hits, should_schedule))
}
/// Parses a "N: translation" line-per-sentence reply back into a
/// source-sentence -> translation map. A line the model garbles or omits is
/// simply left out (that sentence stays uncached and gets retried whenever it
/// is next requested) rather than failing the whole batch.
fn parse_numbered_translations(answer: &str, sentences: &[String]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in answer.lines() {
        let line = line.trim();
        let Some((num, rest)) = line.split_once(':') else {
            continue;
        };
        let Ok(index) = num.trim().parse::<usize>() else {
            continue;
        };
        if index == 0 || index > sentences.len() {
            continue;
        }
        let translation = rest.trim();
        if translation.is_empty() {
            continue;
        }
        out.insert(sentences[index - 1].clone(), translation.to_string());
    }
    out
}
/// One HTTP round for a batch of sentences: builds a numbered prompt, calls
/// the user's configured model through the same streamed_completion path chat
/// and notes use (a no-op channel discards the deltas; only the final text
/// matters here), parses the reply and upserts every sentence it could match
/// into the persisted cache.
async fn translate_batch_impl(
    state: &AppState,
    session_id: &str,
    target_language: &str,
    sentences: &[String],
) -> Result<HashMap<String, String>, String> {
    let key = translation_key(session_id, target_language);
    let token = reserve_token(
        &state.translation_cancellations,
        &key,
        "该语言的翻译任务正在进行。",
    )?;
    let mut user_content = format!(
        "Translate each numbered sentence into the language identified by ISO 639-1 code \"{target_language}\". Reply with exactly one line per sentence in the form \"N: translation\", preserving the original numbering and order, and nothing else.\n\n"
    );
    for (index, sentence) in sentences.iter().enumerate() {
        user_content.push_str(&format!("{}: {}\n", index + 1, sentence));
    }
    let messages = vec![
        json!({"role":"system","content":"You translate sentences from a live classroom transcript. Reply only with the numbered translations in the exact requested format, with no extra commentary."}),
        json!({"role":"user","content":user_content}),
    ];
    let channel = Channel::<StreamEvent>::new(|_| Ok(()));
    let result = streamed_completion(state, session_id, messages, None, channel, token).await;
    state
        .translation_cancellations
        .lock()
        .map_err(|_| "翻译状态不可用。")?
        .remove(&key);
    let (answer, _) = result?;
    let parsed = parse_numbered_translations(&answer, sentences);
    let db = state.db()?;
    for (source, translation) in &parsed {
        db.execute(
            "INSERT INTO sentence_translations(source_text,target_language,translation) VALUES(?,?,?) ON CONFLICT(source_text,target_language) DO UPDATE SET translation=excluded.translation",
            params![source, target_language, translation],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(parsed)
}
/// Result of one batch translation round, for the caller to turn into a
/// "translation-status" event.
struct TranslationBatchResult {
    sentences: Vec<String>,
    translations: HashMap<String, String>,
    error: Option<String>,
}
/// Drives the debounced batch queue for one session+language pair: drains
/// whatever is pending, sends one request for it, and loops immediately
/// (rather than waiting a further debounce) if more sentences queued while
/// that request was in flight - mirroring the notes job's own rerun loop, just
/// triggered by a timer instead of a character threshold. Exits (clearing
/// `scheduled`) once the pending queue is actually empty.
async fn run_translation_batch(
    state: &AppState,
    session_id: &str,
    target_language: &str,
    on_event: &(impl Fn(TranslationBatchResult) + Send + Sync),
) {
    let key = translation_key(session_id, target_language);
    loop {
        let batch = {
            let Ok(mut jobs) = state.translations.lock() else {
                return;
            };
            let Some(entry) = jobs.get_mut(&key) else {
                return;
            };
            if entry.pending.is_empty() {
                jobs.remove(&key);
                return;
            }
            std::mem::take(&mut entry.pending)
        };
        let result = translate_batch_impl(state, session_id, target_language, &batch).await;
        match result {
            // Only success loops back immediately to drain whatever queued
            // while the request was in flight.
            Ok(translations) => {
                if let Ok(mut jobs) = state.translations.lock() {
                    if let Some(entry) = jobs.get_mut(&key) {
                        entry.consecutive_failures = 0;
                    }
                }
                on_event(TranslationBatchResult {
                    sentences: batch,
                    translations,
                    error: None,
                });
            }
            // On error the batch stays queued (retried, never dropped) and
            // the loop waits out a debounce/backoff delay before its next
            // attempt, instead of hammering the provider as fast as new
            // sentences keep arriving.
            Err(error) => {
                let delay = {
                    let Ok(mut jobs) = state.translations.lock() else {
                        return;
                    };
                    let entry = jobs.entry(key.clone()).or_default();
                    for text in batch.iter().rev() {
                        if !entry.pending.iter().any(|existing| existing == text) {
                            entry.pending.insert(0, text.clone());
                        }
                    }
                    entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
                    entry.scheduled = true;
                    translation_backoff_delay(entry.consecutive_failures)
                };
                on_event(TranslationBatchResult {
                    sentences: batch,
                    translations: HashMap::new(),
                    error: Some(error),
                });
                tokio::time::sleep(delay).await;
            }
        }
    }
}
/// Delay before the next retry after a translation batch error: the normal
/// debounce for the first failure, doubling per additional consecutive
/// failure, capped at TRANSLATION_MAX_BACKOFF_MS so a persistent outage never
/// waits longer than that between attempts.
fn translation_backoff_delay(consecutive_failures: u32) -> Duration {
    let factor = 1u64 << consecutive_failures.saturating_sub(1).min(6);
    let millis = TRANSLATION_DEBOUNCE_MS
        .saturating_mul(factor)
        .min(TRANSLATION_MAX_BACKOFF_MS);
    Duration::from_millis(millis)
}
/// Turns one batch result into a "translation-status" event the frontend
/// listens for, matching the sentences it asked about so it can clear their
/// "translating" state regardless of whether that round succeeded.
fn emit_translation_event(
    app: &tauri::AppHandle,
    session_id: &str,
    target_language: &str,
    result: TranslationBatchResult,
) {
    let payload = json!({
        "sessionId": session_id,
        "targetLanguage": target_language,
        "sentences": result.sentences,
        "translations": result.translations,
        "error": result.error,
    });
    let _ = app.emit("translation-status", payload);
}
#[tauri::command]
async fn queue_sentence_translations(
    app: tauri::AppHandle,
    session_id: String,
    target_language: String,
    sentences: Vec<String>,
    state: tauri::State<'_, AppState>,
) -> Result<HashMap<String, String>, String> {
    let target_language = target_language.trim().to_string();
    if target_language.is_empty() {
        return Err("目标语言不能为空。".into());
    }
    let (hits, should_schedule) =
        enqueue_translations(&state, &session_id, &target_language, &sentences)?;
    if should_schedule {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(TRANSLATION_DEBOUNCE_MS)).await;
            let state = app.state::<AppState>();
            let emit_app = app.clone();
            let emit_session_id = session_id.clone();
            let emit_target_language = target_language.clone();
            run_translation_batch(&state, &session_id, &target_language, &move |result| {
                emit_translation_event(&emit_app, &emit_session_id, &emit_target_language, result)
            })
            .await;
        });
    }
    Ok(hits)
}

#[tauri::command]
async fn stt_status(state: tauri::State<'_, AppState>) -> Result<stt::SttStatus, String> {
    state.stt.status().await
}
#[tauri::command]
async fn download_stt_model(
    on_event: Channel<stt::DownloadProgress>,
    state: tauri::State<'_, AppState>,
) -> Result<stt::SttStatus, String> {
    state
        .stt
        .install(move |progress| {
            let _ = on_event.send(progress);
        })
        .await
}
#[tauri::command]
fn cancel_stt_download(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.stt.cancel_download()
}

fn append_transcription(state: &AppState, session_id: &str, text: &str) -> Result<String, String> {
    append_transcription_at(&state.db_path, session_id, text)
}

fn append_transcription_at(
    database_path: &Path,
    session_id: &str,
    text: &str,
) -> Result<String, String> {
    let mut db = open_database(database_path)?;
    let tx = db.transaction().map_err(|e| e.to_string())?;
    let existing = tx
        .query_row(
            "SELECT transcription FROM sessions WHERE id=?",
            [session_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or("找不到会话。")?;
    let combined = match existing.filter(|value| !value.trim().is_empty()) {
        Some(previous) => format!("{previous}\n\n{text}"),
        None => text.to_owned(),
    };
    tx.execute(
        "UPDATE sessions SET transcription=? WHERE id=?",
        params![combined, session_id],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(combined)
}

fn set_transcription_words(
    state: &AppState,
    session_id: &str,
    words: &[stt::Word],
) -> Result<(), String> {
    // Words describe a single audio file's own timeline, so a new transcription
    // replaces rather than appends to whatever the previous one stored.
    let json = if words.is_empty() {
        None
    } else {
        Some(serde_json::to_string(words).map_err(|e| e.to_string())?)
    };
    state
        .db()?
        .execute(
            "UPDATE sessions SET transcription_words=? WHERE id=?",
            params![json, session_id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

async fn transcribe(
    state: &AppState,
    session_id: &str,
    name: String,
    bytes: Vec<u8>,
    language: Option<String>,
) -> Result<String, String> {
    let exists: bool = state
        .db()?
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?)",
            [session_id],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    if !exists {
        return Err("找不到会话。".into());
    }
    let cancellation = generation_token(state, session_id)?;
    let result = state
        .stt
        .transcribe(name, bytes, language, cancellation.clone())
        .await;
    let output = match result {
        Ok(transcription) if !cancellation.is_cancelled() => {
            append_transcription(state, session_id, &transcription.text).and_then(|combined| {
                set_transcription_words(state, session_id, &transcription.words)?;
                Ok(combined)
            })
        }
        Ok(_) => Err("语音任务已取消。".into()),
        Err(error) => Err(error),
    };
    state
        .cancellations
        .lock()
        .map_err(|_| "生成状态不可用。")?
        .remove(session_id);
    output
}
#[tauri::command]
async fn transcribe_audio(
    session_id: String,
    path: String,
    language: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let name = Path::new(&path)
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("recording")
        .to_string();
    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > stt::MAX_AUDIO_BYTES {
        return Err("音频为空、不是普通文件或超过 512 MiB。".into());
    }
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::new();
    tokio::fs::File::open(path)
        .await
        .map_err(|e| e.to_string())?
        .take(stt::MAX_AUDIO_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| e.to_string())?;
    transcribe(&state, &session_id, name, bytes, language).await
}
#[tauri::command]
async fn transcribe_bytes(
    session_id: String,
    name: String,
    bytes: Vec<u8>,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    transcribe(&state, &session_id, name, bytes, None).await
}
#[tauri::command]
async fn read_audio_file(path: String) -> Result<tauri::ipc::Response, String> {
    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > stt::MAX_AUDIO_BYTES {
        return Err("音频为空、不是普通文件或超过 512 MiB。".into());
    }
    let bytes = tokio::fs::read(&path).await.map_err(|e| e.to_string())?;
    Ok(tauri::ipc::Response::new(bytes))
}

#[tauri::command]
async fn start_recording(
    app: tauri::AppHandle,
    session_id: String,
    on_event: Channel<RecordingEvent>,
    source: recording::RecordingSource,
    language: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let exists: bool = state
        .db()?
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?)",
            [&session_id],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    if !exists {
        return Err("找不到会话。".into());
    }
    // Clears any `stopped` mark (and stale cursor/pending) a previous
    // recording on this session left behind, so the background notes job can
    // run for this fresh recording.
    if let Ok(mut jobs) = state.notes.lock() {
        jobs.remove(&session_id);
    }
    let database = state.db_path.clone();
    let active_session = session_id.clone();
    let channel = on_event.clone();
    let notes_app = app.clone();
    let on_transcript = std::sync::Arc::new(move |segment: String| {
        let text = append_transcription_at(&database, &active_session, &segment)?;
        channel
            .send(RecordingEvent::Transcript {
                session_id: active_session.clone(),
                text,
            })
            // A `?` mid-closure needs a concrete error type before it can convert;
            // an unannotated `.into()` here leaves that type ambiguous.
            .map_err(|_| "转写状态通道已关闭。".to_string())?;
        // Fire-and-forget: the recording worker thread must never block on a
        // network call, and this spawned task never touches the synchronous
        // callback path again.
        let app = notes_app.clone();
        let sid = active_session.clone();
        tauri::async_runtime::spawn(async move {
            let state = app.state::<AppState>();
            let emit_app = app.clone();
            let emit_sid = sid.clone();
            on_transcript_appended(&state, &sid, &move |event| {
                emit_notes_event(&emit_app, &emit_sid, event)
            })
            .await;
        });
        Ok(())
    });
    let error_session = session_id.clone();
    let error_channel = on_event.clone();
    let on_error = std::sync::Arc::new(move |text: String| {
        let _ = error_channel.send(RecordingEvent::Error {
            session_id: error_session.clone(),
            text,
        });
    });
    let level_session = session_id.clone();
    let level_channel = on_event.clone();
    let on_level = std::sync::Arc::new(move |level: f32| {
        let _ = level_channel.send(RecordingEvent::Level {
            session_id: level_session.clone(),
            level,
        });
    });
    match state
        .recording
        .start(
            &session_id,
            state.db_path.parent().ok_or("本地数据路径无效。")?,
            &state.stt,
            source,
            language,
            on_transcript,
            on_error,
            on_level,
        )
        .await
    {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = on_event.send(RecordingEvent::Error {
                session_id,
                text: error.clone(),
            });
            Err(error)
        }
    }
}
#[tauri::command]
async fn stop_recording(
    session_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let path = state.recording.stop(&session_id).await?;
    // Recording has stopped, so no more transcript will arrive to fold in;
    // cancel any in-flight or pending background notes/translation job for
    // this session rather than let it write results after the fact.
    stop_notes_for_session(&state, &session_id);
    stop_translation_for_session(&state, &session_id);
    tokio::fs::remove_file(&path)
        .await
        .map_err(|_| "转写完成，但无法清理临时录音。")?;
    state
        .db()?
        .query_row(
            "SELECT COALESCE(transcription, '') FROM sessions WHERE id=?",
            [&session_id],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())
}
#[tauri::command]
async fn cancel_recording(
    session_id: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    if let Some(session_id) = &session_id {
        stop_notes_for_session(&state, session_id);
        stop_translation_for_session(&state, session_id);
    }
    state.recording.cancel().await
}
#[tauri::command]
fn get_system_audio_capability() -> recording::SystemAudioCapability {
    recording::system_audio_capability()
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data = app
                .path()
                .app_data_dir()
                .map_err(|e| e.to_string())?
                .join("async.sqlite3");
            app.manage(AppState::open(data)?);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            auth_commands::get_auth_state,
            auth_commands::model_auth_action,
            auth_commands::cancel_model_auth,
            load_state,
            create_workspace,
            rename_workspace,
            delete_workspace,
            create_session,
            rename_session,
            set_notes_enabled,
            set_translation_settings,
            queue_sentence_translations,
            delete_session,
            save_session,
            import_material,
            save_material,
            delete_material,
            save_settings,
            delete_provider,
            discover_models,
            chat,
            cancel_generation,
            summarize,
            transcribe_audio,
            transcribe_bytes,
            read_audio_file,
            start_recording,
            stop_recording,
            cancel_recording,
            get_system_audio_capability,
            stt_status,
            download_stt_model,
            cancel_stt_download
        ])
        .run(tauri::generate_context!())
        .expect("error while running Async");
}

#[cfg(test)]
#[path = "../../test/backend_core.rs"]
mod backend_core_tests;

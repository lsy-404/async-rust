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
use tauri::{ipc::Channel, Manager};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use zip::ZipArchive;

const CONTEXT_LIMIT: usize = 48_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub id: String,
    pub name: String,
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
      CREATE TABLE IF NOT EXISTS settings(singleton INTEGER PRIMARY KEY CHECK(singleton=1), json TEXT NOT NULL);") .map_err(|e| e.to_string())?;
    // Additive migrations for columns that postdate a user's existing database;
    // guarded so they only ever run once and never touch existing rows.
    ensure_column(&db, "sessions", "summary_updated_at", "TEXT")?;
    ensure_column(&db, "sessions", "transcription_words", "TEXT")?;
    ensure_column(&db, "messages", "created_at", "TEXT NOT NULL DEFAULT ''")?;
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
            "SELECT id,workspace_id,title,transcription,summary,summary_updated_at,transcription_words FROM sessions ORDER BY rowid",
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
        ) = row.map_err(|e| e.to_string())?;
        let transcription_words = transcription_words
            .map(|raw| serde_json::from_str(&raw).map_err(|e| e.to_string()))
            .transpose()?;
        let messages = db
            .prepare(
                "SELECT id,role,content,created_at FROM messages WHERE session_id=? ORDER BY position",
            )
            .map_err(|e| e.to_string())?
            .query_map([&sid], |r| {
                Ok(Message {
                    id: r.get(0)?,
                    role: r.get(1)?,
                    content: r.get(2)?,
                    created_at: r.get(3)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        sessions.push(Session {
            id: sid,
            workspace_id,
            title,
            messages,
            transcription,
            summary,
            summary_updated_at,
            transcription_words,
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
#[tauri::command]
fn delete_session(id: String, state: tauri::State<'_, AppState>) -> Result<(), String> {
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
            "UPDATE sessions SET title=?,transcription=?,summary=?,summary_updated_at=? WHERE id=?",
            params![
                session.title,
                session.transcription,
                session.summary,
                session.summary_updated_at,
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
        tx.execute(
            "INSERT INTO messages(id,session_id,role,content,position,created_at) VALUES(?,?,?,?,?,?)",
            params![m.id, session.id, m.role, m.content, position, created_at],
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
) -> Result<(), String> {
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
        "INSERT INTO messages(id,session_id,role,content,position,created_at) VALUES(?,?,?,?,?,?)",
        params![
            id(),
            session_id,
            role,
            content,
            position,
            chrono::Utc::now().to_rfc3339()
        ],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}
fn generation_token(state: &AppState, session_id: &str) -> Result<CancellationToken, String> {
    let mut active = state.cancellations.lock().map_err(|_| "生成状态不可用。")?;
    if active.contains_key(session_id) {
        return Err("该会话已有生成任务。".into());
    }
    let token = CancellationToken::new();
    active.insert(session_id.into(), token.clone());
    Ok(token)
}
async fn streamed_completion(
    state: &AppState,
    session_id: &str,
    messages: Vec<Value>,
    tool_workspace_id: Option<&str>,
    channel: Channel<StreamEvent>,
    cancellation: CancellationToken,
) -> Result<String, String> {
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
    persist_message(state, session_id, "user", content)?;
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
    let answer = result?;
    if !answer.is_empty() {
        persist_message(state, session_id, "assistant", &answer)?;
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
    let summary = result?;
    let updated_at = chrono::Utc::now().to_rfc3339();
    state
        .db()?
        .execute(
            "UPDATE sessions SET summary=?,summary_updated_at=? WHERE id=?",
            params![summary, updated_at, session_id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
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
    let database = state.db_path.clone();
    let active_session = session_id.clone();
    let channel = on_event.clone();
    let on_transcript = std::sync::Arc::new(move |segment: String| {
        let text = append_transcription_at(&database, &active_session, &segment)?;
        channel
            .send(RecordingEvent::Transcript {
                session_id: active_session.clone(),
                text,
            })
            .map_err(|_| "转写状态通道已关闭。".into())
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
async fn cancel_recording(state: tauri::State<'_, AppState>) -> Result<(), String> {
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

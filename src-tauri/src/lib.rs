mod auth_commands;
mod connections;
mod credential_store;
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
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            provider_id: "openai".into(),
            model: String::new(),
            theme: "system".into(),
            language: "zh".into(),
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
#[derive(Clone, Debug, Serialize)]
pub struct StreamEvent {
    #[serde(rename = "type")]
    pub kind: String,
    pub text: String,
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
      CREATE TABLE IF NOT EXISTS sessions(id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE, title TEXT NOT NULL, transcription TEXT, summary TEXT);
      CREATE TABLE IF NOT EXISTS messages(id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, role TEXT NOT NULL, content TEXT NOT NULL, position INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS materials(id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE, name TEXT NOT NULL, content TEXT NOT NULL, path TEXT);
      CREATE TABLE IF NOT EXISTS providers(id TEXT PRIMARY KEY, name TEXT NOT NULL, base_url TEXT NOT NULL, models_json TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS settings(singleton INTEGER PRIMARY KEY CHECK(singleton=1), json TEXT NOT NULL);") .map_err(|e| e.to_string())?;
    db.execute("INSERT OR IGNORE INTO providers(id,name,base_url,models_json) VALUES('openai','OpenAI','https://api.openai.com/v1','[]')", []).map_err(|e| e.to_string())?;
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
        .prepare("SELECT id,workspace_id,title,transcription,summary FROM sessions ORDER BY rowid")
        .map_err(|e| e.to_string())?;
    let rows = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    for row in rows {
        let (sid, workspace_id, title, transcription, summary) = row.map_err(|e| e.to_string())?;
        let messages = db
            .prepare("SELECT id,role,content FROM messages WHERE session_id=? ORDER BY position")
            .map_err(|e| e.to_string())?
            .query_map([&sid], |r| {
                Ok(Message {
                    id: r.get(0)?,
                    role: r.get(1)?,
                    content: r.get(2)?,
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
    let mut db = state.db()?;
    let tx = db.transaction().map_err(|e| e.to_string())?;
    let updated = tx
        .execute(
            "UPDATE sessions SET title=?,transcription=?,summary=? WHERE id=?",
            params![
                session.title,
                session.transcription,
                session.summary,
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
        tx.execute(
            "INSERT INTO messages(id,session_id,role,content,position) VALUES(?,?,?,?,?)",
            params![m.id, session.id, m.role, m.content, position],
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
fn save_provider(
    mut provider: Provider,
    state: tauri::State<'_, AppState>,
) -> Result<Provider, String> {
    if oauth::is_oauth(&provider.id) {
        return Err("OAuth 供应商地址由授权适配器管理。".into());
    }
    if provider.id.trim().is_empty() || provider.name.trim().is_empty() {
        return Err("提供商 ID 和名称不能为空。".into());
    }
    let parsed = url::Url::parse(provider.base_url.trim()).map_err(|_| "提供商地址无效。")?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err("提供商地址必须是没有凭据、查询参数或片段的 HTTP(S) 地址。".into());
    }
    provider.base_url = parsed.as_str().trim_end_matches('/').into();
    provider.auth_method = "api-key".into();
    state.db()?.execute("INSERT INTO providers(id,name,base_url,models_json) VALUES(?,?,?,'[]') ON CONFLICT(id) DO UPDATE SET name=excluded.name,base_url=excluded.base_url",params![provider.id,provider.name,provider.base_url]).map_err(|e|e.to_string())?;
    provider.has_key = connections::has_credential(&state, &provider.id)?;
    Ok(provider)
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
        "INSERT INTO messages(id,session_id,role,content,position) VALUES(?,?,?,?,?)",
        params![id(), session_id, role, content, position],
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
    channel: Channel<StreamEvent>,
    cancellation: CancellationToken,
) -> Result<String, String> {
    let (_, _, settings) = session_context(state, session_id)?;
    auth_commands::stream(
        state,
        &settings.provider_id,
        &settings.model,
        messages,
        cancellation,
        channel,
    )
    .await
}
#[tauri::command]
async fn chat(
    session_id: String,
    content: String,
    on_event: Channel<StreamEvent>,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let (session, materials, _) = session_context(&state, &session_id)?;
    persist_message(&state, &session_id, "user", &content)?;
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
    let token = generation_token(&state, &session_id)?;
    let result = streamed_completion(&state, &session_id, messages, on_event, token).await;
    state
        .cancellations
        .lock()
        .map_err(|_| "生成状态不可用。")?
        .remove(&session_id);
    let answer = result?;
    if !answer.is_empty() {
        persist_message(&state, &session_id, "assistant", &answer)?;
    }
    Ok(())
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
    let result = streamed_completion(&state, &session_id, messages, on_event, token).await;
    state
        .cancellations
        .lock()
        .map_err(|_| "生成状态不可用。")?
        .remove(&session_id);
    let summary = result?;
    state
        .db()?
        .execute(
            "UPDATE sessions SET summary=? WHERE id=?",
            params![summary, session_id],
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
    let mut db = state.db()?;
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

async fn transcribe(
    state: &AppState,
    session_id: &str,
    name: String,
    bytes: Vec<u8>,
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
        .transcribe(name, bytes, cancellation.clone())
        .await;
    let output = match result {
        Ok(text) if !cancellation.is_cancelled() => append_transcription(state, session_id, &text),
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
    transcribe(&state, &session_id, name, bytes).await
}
#[tauri::command]
async fn transcribe_bytes(
    session_id: String,
    name: String,
    bytes: Vec<u8>,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    transcribe(&state, &session_id, name, bytes).await
}

#[tauri::command]
async fn start_recording(
    session_id: String,
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
    if state
        .cancellations
        .lock()
        .map_err(|_| "任务状态不可用。")?
        .contains_key(&session_id)
    {
        return Err("该会话已有任务运行中。".into());
    }
    state
        .recording
        .start(
            &session_id,
            state.db_path.parent().ok_or("本地数据路径无效。")?,
        )
        .await
}
#[tauri::command]
async fn stop_recording(
    session_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let path = state.recording.stop(&session_id).await?;
    let result = transcribe_audio(session_id, path.to_string_lossy().into_owned(), state).await;
    let cleanup = tokio::fs::remove_file(&path).await;
    match result {
        Ok(text) => {
            cleanup.map_err(|_| "转写完成，但无法清理临时录音。")?;
            Ok(text)
        }
        Err(error) => Err(error),
    }
}
#[tauri::command]
async fn cancel_recording(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.recording.cancel().await
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
            delete_workspace,
            create_session,
            delete_session,
            save_session,
            import_material,
            save_material,
            delete_material,
            save_settings,
            save_provider,
            delete_provider,
            discover_models,
            chat,
            cancel_generation,
            summarize,
            transcribe_audio,
            transcribe_bytes,
            start_recording,
            stop_recording,
            cancel_recording,
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

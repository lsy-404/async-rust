use std::{
    collections::HashMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use keyring::Entry;
use quick_xml::{events::Event, Reader};
use reqwest::multipart;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{ipc::Channel, Manager};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use zip::ZipArchive;

const KEYRING_SERVICE: &str = "dev.async.desktop.api-key";
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
    pub transcription_model: String,
    pub theme: String,
    pub language: String,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            provider_id: "openai".into(),
            model: String::new(),
            transcription_model: "whisper-1".into(),
            theme: "system".into(),
            language: "zh".into(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
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
    client: reqwest::Client,
    cancellations: Mutex<HashMap<String, CancellationToken>>,
}

impl AppState {
    pub fn open(db_path: PathBuf) -> Result<Self, String> {
        let state = Self {
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
        Entry::new(KEYRING_SERVICE, id).map_err(|e| format!("系统凭据库不可用：{e}"))
    }
    fn has_key(&self, id: &str) -> Result<bool, String> {
        match self.key(id)?.get_password() {
            Ok(k) => Ok(!k.trim().is_empty()),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(e) => Err(format!("无法读取系统 API Key：{e}")),
        }
    }
    fn api_key(&self, id: &str) -> Result<String, String> {
        self.key(id)?
            .get_password()
            .map_err(|e| match e {
                keyring::Error::NoEntry => "尚未配置 API Key。".into(),
                _ => format!("无法读取系统 API Key：{e}"),
            })
            .and_then(|v| {
                if v.trim().is_empty() {
                    Err("系统 API Key 为空。".into())
                } else {
                    Ok(v)
                }
            })
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
            has_key: state.has_key(&id)?,
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
    provider: Provider,
    api_key: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<Provider, String> {
    if provider.id.trim().is_empty() || provider.base_url.trim().is_empty() {
        return Err("提供商 ID 和地址不能为空。".into());
    };
    let parsed =
        url::Url::parse(&provider.base_url).map_err(|_| "提供商地址必须是有效的 HTTP(S) URL。")?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err("提供商地址必须是没有凭据、查询参数或片段的 HTTP(S) 地址。".into());
    }
    if let Some(key) = api_key {
        let entry = state.key(&provider.id)?;
        if key.trim().is_empty() {
            match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => {}
                Err(e) => return Err(format!("无法删除系统 API Key：{e}")),
            }
        } else {
            entry
                .set_password(&key)
                .map_err(|e| format!("无法写入系统 API Key：{e}"))?;
        }
    }
    state.db()?.execute("INSERT INTO providers(id,name,base_url,models_json) VALUES(?,?,?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,base_url=excluded.base_url,models_json=excluded.models_json",params![provider.id,provider.name,provider.base_url,serde_json::to_string(&provider.models).map_err(|e|e.to_string())?]).map_err(|e|e.to_string())?;
    Ok(Provider {
        has_key: state.has_key(&provider.id)?,
        ..provider
    })
}
#[tauri::command]
fn delete_provider(id: String, state: tauri::State<'_, AppState>) -> Result<(), String> {
    if id == "openai" {
        return Err("默认 OpenAI 提供商不能删除。".into());
    };
    state
        .db()?
        .execute("DELETE FROM providers WHERE id=?", [&id])
        .map_err(|e| e.to_string())?;
    match state.key(&id)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("无法删除系统 API Key：{e}")),
    }
}
fn endpoint(base: &str, suffix: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/').trim_end_matches("/v1"),
        format!("v1/{suffix}").trim_start_matches("v1/v1/")
    )
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
    let p = provider(&state, &provider_id)?;
    let key = state.api_key(&provider_id)?;
    let url = endpoint(&p.base_url, "models");
    let value: Value = state
        .client
        .get(url)
        .bearer_auth(key)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    let mut models = value
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| v.get("id").and_then(Value::as_str).map(ToOwned::to_owned))
        .collect::<Vec<_>>();
    models.sort();
    models.dedup();
    state
        .db()?
        .execute(
            "UPDATE providers SET models_json=? WHERE id=?",
            params![
                serde_json::to_string(&models).map_err(|e| e.to_string())?,
                provider_id
            ],
        )
        .map_err(|e| e.to_string())?;
    Ok(models)
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
fn sse_delta(data: &str) -> Result<Option<String>, String> {
    if data == "[DONE]" {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(data).map_err(|_| "模型流返回了无效事件。")?;
    if let Some(message) = value.pointer("/error/message").and_then(Value::as_str) {
        return Err(format!("模型请求失败：{message}"));
    }
    Ok(value
        .pointer("/choices/0/delta/content")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned))
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
    let p = provider(state, &settings.provider_id)?;
    if settings.model.trim().is_empty() {
        return Err("请先选择聊天模型。".into());
    };
    let key = state.api_key(&p.id)?;
    let request = state
        .client
        .post(endpoint(&p.base_url, "chat/completions"))
        .bearer_auth(key)
        .json(&json!({"model":settings.model,"messages":messages,"stream":true}));
    let response = tokio::select! { _ = cancellation.cancelled() => return Ok(String::new()), response = request.send() => response.map_err(|e|e.to_string())? }
        .error_for_status()
        .map_err(|e| e.to_string())?;
    let mut stream = response.bytes_stream().eventsource();
    let mut answer = String::new();
    let mut complete = false;
    while !complete {
        let next =
            tokio::select! { _ = cancellation.cancelled() => break, next = stream.next() => next };
        let Some(next) = next else {
            break;
        };
        let event = next.map_err(|e| e.to_string())?;
        if event.data == "[DONE]" {
            complete = true;
            continue;
        }
        if let Some(delta) = sse_delta(&event.data)? {
            answer.push_str(&delta);
            channel
                .send(StreamEvent {
                    kind: "delta".into(),
                    text: delta,
                })
                .map_err(|e| e.to_string())?;
        }
    }
    channel
        .send(StreamEvent {
            kind: "done".into(),
            text: String::new(),
        })
        .map_err(|e| e.to_string())?;
    Ok(answer)
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

async fn transcribe(
    state: &AppState,
    session_id: &str,
    name: String,
    bytes: Vec<u8>,
) -> Result<String, String> {
    let (_, _, settings) = session_context(state, session_id)?;
    let p = provider(state, &settings.provider_id)?;
    let key = state.api_key(&p.id)?;
    let mime = match Path::new(&name)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "ogg" => "audio/ogg",
        "webm" => "audio/webm",
        _ => "application/octet-stream",
    };
    let part = multipart::Part::bytes(bytes)
        .file_name(name)
        .mime_str(mime)
        .map_err(|e| e.to_string())?;
    let form = multipart::Form::new()
        .text("model", settings.transcription_model)
        .part("file", part);
    let value: Value = state
        .client
        .post(endpoint(&p.base_url, "audio/transcriptions"))
        .bearer_auth(key)
        .multipart(form)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    let text = value
        .get("text")
        .and_then(Value::as_str)
        .ok_or("转录服务未返回 text。")?
        .to_string();
    let db = state.db()?;
    let existing = db
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
        None => text,
    };
    db.execute(
        "UPDATE sessions SET transcription=? WHERE id=?",
        params![combined, session_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(combined)
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
    let bytes = tokio::fs::read(path).await.map_err(|e| e.to_string())?;
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
            transcribe_bytes
        ])
        .run(tauri::generate_context!())
        .expect("error while running Async");
}

#[cfg(test)]
#[path = "../../test/backend_core.rs"]
mod backend_core_tests;

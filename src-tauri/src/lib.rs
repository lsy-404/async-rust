mod auth_commands;
mod connections;
mod credential_store;
mod models_catalog;
mod oauth;
mod recording;
mod schema;
mod stt;
mod text_match;

use std::{
    collections::{HashMap, HashSet},
    fs,
    future::Future,
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
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
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
// search_library caps its result count at this many hits (spec: "measure").
const SEARCH_RESULT_LIMIT: usize = 100;
const SEARCH_EXCERPT_RADIUS: usize = 60;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NodeKind {
    Folder,
    Session,
    Material,
}
impl NodeKind {
    fn as_str(self) -> &'static str {
        match self {
            NodeKind::Folder => "folder",
            NodeKind::Session => "session",
            NodeKind::Material => "material",
        }
    }
}
fn parse_node_kind(value: &str) -> Result<NodeKind, String> {
    match value {
        "folder" => Ok(NodeKind::Folder),
        "session" => Ok(NodeKind::Session),
        "material" => Ok(NodeKind::Material),
        other => Err(format!("未知节点类型：{other}")),
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    pub id: String,
    pub parent_id: Option<String>,
    pub kind: NodeKind,
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
    // Persisted expanded-folder ids for the explorer tree view.
    #[serde(default)]
    pub explorer_expanded: Vec<String>,
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
            explorer_expanded: Vec::new(),
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
    pub nodes: Vec<Node>,
    pub sessions: Vec<Session>,
    pub materials: Vec<Material>,
    pub settings: Settings,
    pub providers: Vec<Provider>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum SearchField {
    Name,
    Transcription,
    Summary,
    Content,
    Message,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchPiece {
    pub before: String,
    pub matched: String,
    pub after: String,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub node_id: String,
    pub kind: NodeKind,
    pub field: SearchField,
    pub piece: SearchPiece,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResults {
    pub hits: Vec<SearchHit>,
    pub truncated: bool,
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
    pub fn open(db_path: PathBuf) -> Result<Self, schema::InitError> {
        schema::initialize_database(&db_path)?;
        let stt_root = db_path
            .parent()
            .ok_or_else(|| schema::InitError::from("本地数据路径无效。".to_string()))?
            .join("voice-models");
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| schema::InitError::from(e.to_string()))?;
        Ok(Self {
            stt: stt::SttManager::new(stt_root),
            recording: recording::RecordingManager::new(),
            db_path,
            client,
            cancellations: Mutex::new(HashMap::new()),
            notes_cancellations: Mutex::new(HashMap::new()),
            notes: Mutex::new(HashMap::new()),
            translation_cancellations: Mutex::new(HashMap::new()),
            translations: Mutex::new(HashMap::new()),
        })
    }
    pub fn database_path(&self) -> &Path {
        &self.db_path
    }
    fn db(&self) -> Result<Connection, String> {
        schema::open_connection(&self.db_path)
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

fn id() -> String {
    Uuid::new_v4().to_string()
}
fn limit(value: String) -> String {
    if value.len() <= CONTEXT_LIMIT {
        return value;
    }
    value[..value.floor_char_boundary(CONTEXT_LIMIT)].to_string()
}
/// Trim, require non-empty, at most 255 chars, no control characters.
fn normalize_name(input: &str) -> Result<String, String> {
    let trimmed = input.trim();
    if trimmed.is_empty()
        || trimmed.chars().count() > 255
        || trimmed.chars().any(|c| c.is_control())
    {
        return Err("名称不能为空、不能超过 255 个字符，也不能包含控制字符。".into());
    }
    Ok(trimmed.to_string())
}
fn row_material(row: &rusqlite::Row<'_>) -> rusqlite::Result<Material> {
    Ok(Material {
        id: row.get(0)?,
        content: row.get(1)?,
        path: row.get(2)?,
    })
}
fn load_messages(db: &Connection, session_id: &str) -> Result<Vec<Message>, String> {
    db.prepare(
        "SELECT id,role,content,created_at,tool_calls FROM messages WHERE session_id=?1 ORDER BY position",
    )
    .map_err(|e| e.to_string())?
    .query_map([session_id], |r| {
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
    .collect()
}
#[allow(clippy::too_many_arguments)]
fn session_row_to_session(
    db: &Connection,
    id: String,
    transcription: Option<String>,
    summary: Option<String>,
    summary_updated_at: Option<String>,
    transcription_words: Option<String>,
    notes_enabled: bool,
    translation_enabled: bool,
    translation_target_language: Option<String>,
    translation_mode: String,
) -> Result<Session, String> {
    let transcription_words = transcription_words
        .map(|raw| serde_json::from_str(&raw).map_err(|e| e.to_string()))
        .transpose()?;
    let messages = load_messages(db, &id)?;
    Ok(Session {
        id,
        messages,
        transcription,
        summary,
        summary_updated_at,
        transcription_words,
        notes_enabled,
        translation_enabled,
        translation_target_language,
        translation_mode,
    })
}
fn load_session(db: &Connection, session_id: &str) -> Result<Option<Session>, String> {
    let row = db
        .query_row(
            "SELECT id,transcription,summary,summary_updated_at,transcription_words,notes_enabled,translation_enabled,translation_target_language,translation_mode FROM sessions WHERE id=?1",
            [session_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, bool>(5)?,
                    r.get::<_, bool>(6)?,
                    r.get::<_, Option<String>>(7)?,
                    r.get::<_, String>(8)?,
                ))
            },
        )
        .optional()
        .map_err(|e| e.to_string())?;
    row.map(
        |(
            id,
            transcription,
            summary,
            summary_updated_at,
            transcription_words,
            notes_enabled,
            translation_enabled,
            translation_target_language,
            translation_mode,
        )| {
            session_row_to_session(
                db,
                id,
                transcription,
                summary,
                summary_updated_at,
                transcription_words,
                notes_enabled,
                translation_enabled,
                translation_target_language,
                translation_mode,
            )
        },
    )
    .transpose()
}
fn node_by_id(db: &Connection, id: &str) -> Result<Option<Node>, String> {
    let row = db
        .query_row(
            "SELECT id,parent_id,kind,name FROM nodes WHERE id=?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )
        .optional()
        .map_err(|e| e.to_string())?;
    row.map(|(id, parent_id, kind, name)| {
        Ok(Node {
            id,
            parent_id,
            kind: parse_node_kind(&kind)?,
            name,
        })
    })
    .transpose()
}
fn subtree_ids(db: &Connection, id: &str) -> Result<Vec<String>, String> {
    db.prepare(
        "WITH RECURSIVE sub(id) AS (SELECT id FROM nodes WHERE id=?1 UNION ALL SELECT n.id FROM nodes n JOIN sub s ON n.parent_id=s.id) SELECT id FROM sub",
    )
    .map_err(|e| e.to_string())?
    .query_map([id], |r| r.get::<_, String>(0))
    .map_err(|e| e.to_string())?
    .collect::<Result<Vec<_>, _>>()
    .map_err(|e| e.to_string())
}

fn load_data(state: &AppState) -> Result<AppData, String> {
    let db = state.db()?;
    let nodes = db
        .prepare("SELECT id,parent_id,kind,name FROM nodes ORDER BY rowid")
        .map_err(|e| e.to_string())?
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|(id, parent_id, kind, name)| {
            Ok(Node {
                id,
                parent_id,
                kind: parse_node_kind(&kind)?,
                name,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let mut sessions = Vec::new();
    let mut statement = db
        .prepare(
            "SELECT id,transcription,summary,summary_updated_at,transcription_words,notes_enabled,translation_enabled,translation_target_language,translation_mode FROM sessions ORDER BY rowid",
        )
        .map_err(|e| e.to_string())?;
    let rows = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, bool>(5)?,
                r.get::<_, bool>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, String>(8)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    for row in rows {
        let (
            sid,
            transcription,
            summary,
            summary_updated_at,
            transcription_words,
            notes_enabled,
            translation_enabled,
            translation_target_language,
            translation_mode,
        ) = row.map_err(|e| e.to_string())?;
        sessions.push(session_row_to_session(
            &db,
            sid,
            transcription,
            summary,
            summary_updated_at,
            transcription_words,
            notes_enabled,
            translation_enabled,
            translation_target_language,
            translation_mode,
        )?);
    }
    let materials = db
        .prepare("SELECT id,content,path FROM materials ORDER BY rowid")
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
        nodes,
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

/// Shared create path for `create_folder`, `create_session` and
/// `import_material`: validates the name, validates the parent (when given)
/// is an existing folder, inserts the node, then lets the caller insert its
/// payload row in the same transaction.
fn create_node(
    state: &AppState,
    parent_id: Option<&str>,
    name: &str,
    kind: NodeKind,
    insert_payload: impl FnOnce(&rusqlite::Transaction, &str) -> Result<(), String>,
) -> Result<Node, String> {
    let name = normalize_name(name)?;
    let mut db = state.db()?;
    let tx = db.transaction().map_err(|e| e.to_string())?;
    if let Some(parent) = parent_id {
        let is_folder: Option<bool> = tx
            .query_row(
                "SELECT kind='folder' FROM nodes WHERE id=?1",
                [parent],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if is_folder != Some(true) {
            return Err("目标不是文件夹。".into());
        }
    }
    let new_id = id();
    tx.execute(
        "INSERT INTO nodes(id,parent_id,parent_kind,kind,name) VALUES(?1,?2,CASE WHEN ?2 IS NULL THEN NULL ELSE 'folder' END,?3,?4)",
        params![new_id, parent_id, kind.as_str(), name],
    )
    .map_err(|e| e.to_string())?;
    insert_payload(&tx, &new_id)?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(Node {
        id: new_id,
        parent_id: parent_id.map(str::to_string),
        kind,
        name,
    })
}
#[tauri::command]
fn create_folder(
    parent_id: Option<String>,
    name: String,
    state: tauri::State<'_, AppState>,
) -> Result<Node, String> {
    create_node(
        &state,
        parent_id.as_deref(),
        &name,
        NodeKind::Folder,
        |_, _| Ok(()),
    )
}
#[tauri::command]
fn create_session(
    parent_id: Option<String>,
    name: String,
    state: tauri::State<'_, AppState>,
) -> Result<Node, String> {
    create_node(
        &state,
        parent_id.as_deref(),
        &name,
        NodeKind::Session,
        |tx, new_id| {
            tx.execute("INSERT INTO sessions(id) VALUES(?1)", [new_id])
                .map_err(|e| e.to_string())?;
            Ok(())
        },
    )
}
fn rename_node_impl(state: &AppState, id: &str, name: &str) -> Result<Node, String> {
    let name = normalize_name(name)?;
    let db = state.db()?;
    let updated = db
        .execute("UPDATE nodes SET name=?1 WHERE id=?2", params![name, id])
        .map_err(|e| e.to_string())?;
    if updated == 0 {
        return Err("找不到项目。".into());
    }
    node_by_id(&db, id)?.ok_or_else(|| "找不到项目。".into())
}
#[tauri::command]
fn rename_node(
    id: String,
    name: String,
    state: tauri::State<'_, AppState>,
) -> Result<Node, String> {
    rename_node_impl(&state, &id, &name)
}
fn move_node_impl(state: &AppState, id: &str, parent_id: Option<&str>) -> Result<Node, String> {
    let mut db = state.db()?;
    let tx = db.transaction().map_err(|e| e.to_string())?;
    let current_parent: Option<Option<String>> = tx
        .query_row("SELECT parent_id FROM nodes WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .optional()
        .map_err(|e| e.to_string())?;
    let Some(current_parent) = current_parent else {
        return Err("找不到项目。".into());
    };
    if let Some(parent) = parent_id {
        let is_folder: Option<bool> = tx
            .query_row(
                "SELECT kind='folder' FROM nodes WHERE id=?1",
                [parent],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if is_folder != Some(true) {
            return Err("目标不是文件夹。".into());
        }
        let would_cycle: bool = tx
            .query_row(
                "WITH RECURSIVE up(id) AS (SELECT ?1 UNION SELECT n.parent_id FROM nodes n JOIN up ON n.id=up.id WHERE n.parent_id IS NOT NULL) SELECT EXISTS(SELECT 1 FROM up WHERE id=?2)",
                params![parent, id],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if would_cycle {
            return Err("不能移动到自身或其子文件夹中。".into());
        }
    }
    if current_parent.as_deref() != parent_id {
        tx.execute(
            "UPDATE nodes SET parent_id=?1, parent_kind=CASE WHEN ?1 IS NULL THEN NULL ELSE 'folder' END WHERE id=?2",
            params![parent_id, id],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    node_by_id(&state.db()?, id)?.ok_or_else(|| "找不到项目。".into())
}
#[tauri::command]
fn move_node(
    id: String,
    parent_id: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<Node, String> {
    move_node_impl(&state, &id, parent_id.as_deref())
}
/// The recording/generation busy guard shared by `delete_node`: every session
/// id currently recording or running a chat/summary/file-transcription job.
async fn busy_session_ids(state: &AppState) -> Result<HashSet<String>, String> {
    let mut busy = HashSet::new();
    if let Some(active) = state.recording.active_session_id().await {
        busy.insert(active);
    }
    let cancelling = state
        .cancellations
        .lock()
        .map_err(|_| "生成状态不可用。")?
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    busy.extend(cancelling);
    Ok(busy)
}
async fn delete_node_impl(state: &AppState, id: &str) -> Result<(), String> {
    let subtree = subtree_ids(&state.db()?, id)?;
    if subtree.is_empty() {
        return Err("找不到项目。".into());
    }
    let busy = busy_session_ids(state).await?;
    if subtree.iter().any(|nid| busy.contains(nid)) {
        return Err("该项目中有会话正在录音或生成，请先停止。".into());
    }
    let deleted = state
        .db()?
        .execute("DELETE FROM nodes WHERE id=?1", [id])
        .map_err(|e| e.to_string())?;
    if deleted == 0 {
        return Err("找不到项目。".into());
    }
    stop_notes_for_session(state, id);
    stop_translation_for_session(state, id);
    Ok(())
}
#[tauri::command]
async fn delete_node(id: String, state: tauri::State<'_, AppState>) -> Result<(), String> {
    delete_node_impl(&state, &id).await
}
fn save_messages_impl(
    state: &AppState,
    session_id: &str,
    messages: &[Message],
) -> Result<(), String> {
    let mut db = state.db()?;
    let tx = db.transaction().map_err(|e| e.to_string())?;
    let exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1)",
            [session_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if !exists {
        return Err("找不到会话。".into());
    }
    tx.execute("DELETE FROM messages WHERE session_id=?1", [session_id])
        .map_err(|e| e.to_string())?;
    for (position, m) in messages.iter().enumerate() {
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
            "INSERT INTO messages(id,session_id,position,role,content,created_at,tool_calls) VALUES(?,?,?,?,?,?,?)",
            params![m.id, session_id, position, m.role, m.content, created_at, tool_calls],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())
}
#[tauri::command]
fn save_messages(
    session_id: String,
    messages: Vec<Message>,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    save_messages_impl(&state, &session_id, &messages)
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
fn save_settings(settings: Settings, state: tauri::State<'_, AppState>) -> Result<(), String> {
    let json = serde_json::to_string(&settings).map_err(|e| e.to_string())?;
    state.db()?.execute("INSERT INTO settings(singleton,json) VALUES(1,?) ON CONFLICT(singleton) DO UPDATE SET json=excluded.json",[json]).map_err(|e|e.to_string())?;
    Ok(())
}
#[tauri::command]
fn save_material(
    id: String,
    content: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let updated = state
        .db()?
        .execute(
            "UPDATE materials SET content=?1 WHERE id=?2",
            params![content, id],
        )
        .map_err(|e| e.to_string())?;
    if updated == 0 {
        return Err("找不到材料。".into());
    }
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
    parent_id: Option<String>,
    path: String,
    state: tauri::State<'_, AppState>,
) -> Result<Node, String> {
    let path_buf = PathBuf::from(&path);
    let content = read_material(&path_buf)?;
    let file_name = path_buf.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let name = normalize_name(file_name).unwrap_or_else(|_| "未命名材料".to_string());
    create_node(
        &state,
        parent_id.as_deref(),
        &name,
        NodeKind::Material,
        |tx, new_id| {
            tx.execute(
                "INSERT INTO materials(id,content,path) VALUES(?1,?2,?3)",
                params![new_id, content, path],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        },
    )
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

/// One material reachable from a session's context-injection chain: just the
/// name and content chat/summarize need, decoupled from the node id so the
/// caller never has to look the node back up.
struct ContextMaterial {
    name: String,
    content: String,
}
/// Context-scoping rule (see agents/008.../spec.md "上下文作用域"): a root
/// session sees only root-level materials; a nested session sees the
/// materials of every folder from its own parent up to (but not including)
/// root, nearest folder first, insertion order within a folder.
fn context_materials(db: &Connection, session_id: &str) -> Result<Vec<ContextMaterial>, String> {
    let parent_id: Option<String> = db
        .query_row(
            "SELECT parent_id FROM nodes WHERE id=?1",
            [session_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let rows: Vec<(String, String)> = if parent_id.is_none() {
        db.prepare(
            "SELECT n.name, m.content FROM nodes n JOIN materials m ON m.id = n.id WHERE n.parent_id IS NULL AND n.kind = 'material' ORDER BY n.rowid",
        )
        .map_err(|e| e.to_string())?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?
    } else {
        db.prepare(
            "WITH RECURSIVE chain(id, dist) AS (
               SELECT parent_id, 0 FROM nodes WHERE id = ?1 AND parent_id IS NOT NULL
               UNION ALL
               SELECT n.parent_id, c.dist + 1 FROM nodes n JOIN chain c ON n.id = c.id WHERE n.parent_id IS NOT NULL
             )
             SELECT n.name, m.content FROM chain c
             JOIN nodes n ON n.parent_id = c.id AND n.kind = 'material'
             JOIN materials m ON m.id = n.id
             ORDER BY c.dist, n.rowid",
        )
        .map_err(|e| e.to_string())?
        .query_map([session_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?
    };
    Ok(rows
        .into_iter()
        .map(|(name, content)| ContextMaterial { name, content })
        .collect())
}
fn session_context(
    state: &AppState,
    session_id: &str,
) -> Result<(Session, Vec<ContextMaterial>, Settings), String> {
    let db = state.db()?;
    let session = load_session(&db, session_id)?.ok_or("找不到会话。")?;
    let materials = context_materials(&db, session_id)?;
    let settings = db
        .query_row("SELECT json FROM settings WHERE singleton=1", [], |r| {
            r.get::<_, String>(0)
        })
        .optional()
        .map_err(|e| e.to_string())?
        .map(|raw| serde_json::from_str(&raw).map_err(|e| e.to_string()))
        .transpose()?
        .unwrap_or_default();
    Ok((session, materials, settings))
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
/// Reads the shared `stopped` flag directly, for a round that must re-check it
/// after the gap between `on_transcript_appended` releasing `state.notes` and
/// this round registering its own cancellation token (see `generate_notes_impl`).
fn notes_stopped(state: &AppState, session_id: &str) -> bool {
    state
        .notes
        .lock()
        .ok()
        .and_then(|jobs| jobs.get(session_id).map(|entry| entry.stopped))
        .unwrap_or(false)
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
    tool_session_id: Option<&str>,
    channel: Channel<StreamEvent>,
    cancellation: CancellationToken,
) -> Result<(String, Vec<ToolCall>), String> {
    let (_, _, settings) = session_context(state, session_id)?;
    auth_commands::stream(
        state,
        &settings.provider_id,
        &settings.model,
        messages,
        tool_session_id,
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
        Some(session_id),
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
    // `on_transcript_appended` commits `running=true` under `state.notes` and
    // releases that lock before this call reserves its own token in the
    // separate `notes_cancellations` map, so a `stop_notes_for_session` landing
    // in that gap finds no token to cancel. Re-check the flag directly here so
    // the round bails out before spending a model call on a session that was
    // stopped out from under it.
    if notes_stopped(state, session_id) {
        state
            .notes_cancellations
            .lock()
            .map_err(|_| "笔记生成状态不可用。")?
            .remove(session_id);
        return Ok(());
    }
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
    persist_notes_unless_stopped(state, session_id, &notes)
}
/// Re-checks `stopped` and writes the notes while still holding `state.notes`,
/// the same lock `stop_notes_for_session` takes to set that flag, so a stop
/// landing after the model call finishes still wins the write instead of
/// racing this round's `persist_notes` (see `generate_notes_impl`).
fn persist_notes_unless_stopped(
    state: &AppState,
    session_id: &str,
    notes: &str,
) -> Result<(), String> {
    let jobs = state.notes.lock().map_err(|_| "笔记生成状态不可用。")?;
    if jobs
        .get(session_id)
        .map(|entry| entry.stopped)
        .unwrap_or(false)
    {
        return Ok(());
    }
    persist_notes(state, session_id, notes)
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
    let mut db = schema::open_connection(database_path)?;
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

/// Holds exactly the callback construction and `RecordingManager::start` call
/// that both `start_recording` and `start_quick_transcription` need. Unlike
/// the spec's internal-function list, this also takes `app` (not just
/// `state`): the transcript callback must spawn the (post-spec) background
/// notes job via `app.state::<AppState>()`, which needs an owned, 'static
/// handle rather than the caller's borrowed `&AppState`.
async fn begin_recording(
    app: tauri::AppHandle,
    state: &AppState,
    session_id: &str,
    source: recording::RecordingSource,
    language: Option<String>,
    on_event: Channel<RecordingEvent>,
) -> Result<(), String> {
    // Clears any `stopped` mark (and stale cursor/pending) a previous
    // recording on this session left behind, so the background notes job can
    // run for this fresh recording.
    if let Ok(mut jobs) = state.notes.lock() {
        jobs.remove(session_id);
    }
    let database = state.db_path.clone();
    let active_session = session_id.to_string();
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
    let error_session = session_id.to_string();
    let error_channel = on_event.clone();
    let on_error = std::sync::Arc::new(move |text: String| {
        let _ = error_channel.send(RecordingEvent::Error {
            session_id: error_session.clone(),
            text,
        });
    });
    let level_session = session_id.to_string();
    let level_channel = on_event.clone();
    let on_level = std::sync::Arc::new(move |level: f32| {
        let _ = level_channel.send(RecordingEvent::Level {
            session_id: level_session.clone(),
            level,
        });
    });
    state
        .recording
        .start(
            session_id,
            state.db_path.parent().ok_or("本地数据路径无效。")?,
            &state.stt,
            source,
            language,
            on_transcript,
            on_error,
            on_level,
        )
        .await
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
    match begin_recording(app, &state, &session_id, source, language, on_event.clone()).await {
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
/// Inserts the root session (node + payload) in one transaction, awaits
/// `start`, and on error compensates by deleting the node it just inserted -
/// so a failed start never leaves an empty session behind.
async fn create_then_start<F, Fut>(
    state: &AppState,
    id: &str,
    name: &str,
    start: F,
) -> Result<Node, String>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    {
        let mut db = state.db()?;
        let tx = db.transaction().map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO nodes(id,parent_id,parent_kind,kind,name) VALUES(?1,NULL,NULL,'session',?2)",
            params![id, name],
        )
        .map_err(|e| {
            if e.to_string().to_lowercase().contains("unique") {
                "会话已存在。".to_string()
            } else {
                e.to_string()
            }
        })?;
        tx.execute("INSERT INTO sessions(id) VALUES(?1)", [id])
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
    }
    match start().await {
        Ok(()) => Ok(Node {
            id: id.to_string(),
            parent_id: None,
            kind: NodeKind::Session,
            name: name.to_string(),
        }),
        Err(error) => {
            let cleanup = schema::open_connection(&state.db_path).and_then(|db| {
                db.execute("DELETE FROM nodes WHERE id=?1 AND kind='session'", [id])
                    .map_err(|e| e.to_string())
            });
            match cleanup {
                Ok(_) => Err(error),
                Err(cleanup_error) => Err(format!("{error}；清理空会话失败：{cleanup_error}")),
            }
        }
    }
}
#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn start_quick_transcription(
    app: tauri::AppHandle,
    id: String,
    name: String,
    source: recording::RecordingSource,
    language: Option<String>,
    on_event: Channel<RecordingEvent>,
    state: tauri::State<'_, AppState>,
) -> Result<Node, String> {
    if Uuid::parse_str(&id).is_err() {
        return Err("会话标识无效。".into());
    }
    let name = normalize_name(&name)?;
    if matches!(source, recording::RecordingSource::SystemAudio)
        && !recording::system_audio_capability().available
    {
        return Err("当前系统不支持系统音频采集。".into());
    }
    if state.recording.active_session_id().await.is_some() {
        return Err("已有正在进行的录音，请先停止或取消。".into());
    }
    if !state.stt.status().await?.ready {
        return Err("本地语音模型尚未就绪，请先下载。".into());
    }
    create_then_start(&state, &id, &name, || {
        begin_recording(app, &state, &id, source, language, on_event)
    })
    .await
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

fn search_library_impl(state: &AppState, query: &str) -> Result<SearchResults, String> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(SearchResults {
            hits: vec![],
            truncated: false,
        });
    }
    let db = state.db()?;
    let pattern = format!("%{}%", text_match::escape_like(q));
    let mut hits = Vec::new();

    let mut names_stmt = db
        .prepare("SELECT id,kind,name FROM nodes WHERE name LIKE ?1 ESCAPE '\\' ORDER BY rowid")
        .map_err(|e| e.to_string())?;
    let names = names_stmt
        .query_map(params![pattern], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    for (node_id, kind, name) in names {
        if let Some(piece) = text_match::piece(&name, q, SEARCH_EXCERPT_RADIUS) {
            hits.push(SearchHit {
                node_id,
                kind: parse_node_kind(&kind)?,
                field: SearchField::Name,
                piece,
            });
        }
    }

    let mut transcription_stmt = db
        .prepare("SELECT id,transcription FROM sessions WHERE transcription LIKE ?1 ESCAPE '\\' ORDER BY rowid")
        .map_err(|e| e.to_string())?;
    let transcriptions = transcription_stmt
        .query_map(params![pattern], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    for (node_id, transcription) in transcriptions {
        if let Some(piece) = text_match::piece(&transcription, q, SEARCH_EXCERPT_RADIUS) {
            hits.push(SearchHit {
                node_id,
                kind: NodeKind::Session,
                field: SearchField::Transcription,
                piece,
            });
        }
    }

    let mut summary_stmt = db
        .prepare("SELECT id,summary FROM sessions WHERE summary LIKE ?1 ESCAPE '\\' ORDER BY rowid")
        .map_err(|e| e.to_string())?;
    let summaries = summary_stmt
        .query_map(params![pattern], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    for (node_id, summary) in summaries {
        if let Some(piece) = text_match::piece(&summary, q, SEARCH_EXCERPT_RADIUS) {
            hits.push(SearchHit {
                node_id,
                kind: NodeKind::Session,
                field: SearchField::Summary,
                piece,
            });
        }
    }

    let mut content_stmt = db
        .prepare(
            "SELECT id,content FROM materials WHERE content LIKE ?1 ESCAPE '\\' ORDER BY rowid",
        )
        .map_err(|e| e.to_string())?;
    let contents = content_stmt
        .query_map(params![pattern], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    for (node_id, content) in contents {
        if let Some(piece) = text_match::piece(&content, q, SEARCH_EXCERPT_RADIUS) {
            hits.push(SearchHit {
                node_id,
                kind: NodeKind::Material,
                field: SearchField::Content,
                piece,
            });
        }
    }

    // First matching message per session only.
    let mut messages_stmt = db
        .prepare(
            "SELECT session_id,content FROM messages WHERE content LIKE ?1 ESCAPE '\\' ORDER BY session_id, position",
        )
        .map_err(|e| e.to_string())?;
    let messages = messages_stmt
        .query_map(params![pattern], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let mut seen_sessions = HashSet::new();
    for (session_id, content) in messages {
        if !seen_sessions.insert(session_id.clone()) {
            continue;
        }
        if let Some(piece) = text_match::piece(&content, q, SEARCH_EXCERPT_RADIUS) {
            hits.push(SearchHit {
                node_id: session_id,
                kind: NodeKind::Session,
                field: SearchField::Message,
                piece,
            });
        }
    }
    let truncated = hits.len() > SEARCH_RESULT_LIMIT;
    hits.truncate(SEARCH_RESULT_LIMIT);
    Ok(SearchResults { hits, truncated })
}
#[tauri::command]
fn search_library(
    query: String,
    state: tauri::State<'_, AppState>,
) -> Result<SearchResults, String> {
    search_library_impl(&state, &query)
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
            match AppState::open(data) {
                Ok(state) => {
                    app.manage(state);
                }
                Err(error) => {
                    eprintln!("{}", error.message);
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.hide();
                    }
                    let mut text = format!("Async 无法启动：{}", error.message);
                    if let Some(backup) = &error.backup {
                        text.push_str(&format!("\n备份文件：{}", backup.display()));
                    }
                    let handle = app.handle().clone();
                    app.dialog()
                        .message(text)
                        .title("Async")
                        .kind(MessageDialogKind::Error)
                        .buttons(MessageDialogButtons::Ok)
                        .show(move |_| handle.exit(1));
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            auth_commands::get_auth_state,
            auth_commands::model_auth_action,
            auth_commands::cancel_model_auth,
            load_state,
            create_folder,
            create_session,
            rename_node,
            move_node,
            delete_node,
            set_notes_enabled,
            set_translation_settings,
            queue_sentence_translations,
            save_messages,
            import_material,
            save_material,
            save_settings,
            delete_provider,
            discover_models,
            search_library,
            start_quick_transcription,
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

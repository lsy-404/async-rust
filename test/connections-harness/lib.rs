use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf, sync::Mutex};
use tokio_util::sync::CancellationToken;
#[path = "../../src-tauri/src/auth_commands.rs"]
mod auth_commands;
#[path = "../../src-tauri/src/connections.rs"]
mod connections;
#[path = "../../src-tauri/src/credential_store.rs"]
mod credential_store;
#[path = "../../src-tauri/src/oauth/mod.rs"]
mod oauth;
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Settings {
    provider_id: String,
    model: String,
    theme: String,
    language: String,
}
#[derive(Clone, Debug, Serialize)]
struct Provider {
    id: String,
    name: String,
    base_url: String,
    models: Vec<String>,
}
struct AppData {
    providers: Vec<Provider>,
    settings: Settings,
}
#[derive(Clone, Serialize)]
struct StreamEvent {
    kind: String,
    text: String,
}
struct AppState {
    db_path: PathBuf,
    client: reqwest::Client,
    cancellations: Mutex<HashMap<String, CancellationToken>>,
}
impl AppState {
    fn open(db_path: PathBuf) -> Result<Self, String> {
        let state = Self {
            db_path,
            client: reqwest::Client::new(),
            cancellations: Mutex::new(HashMap::new()),
        };
        state.db()?;
        Ok(state)
    }
    fn db(&self) -> Result<Connection, String> {
        let db = Connection::open(&self.db_path).map_err(|e| e.to_string())?;
        db.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| e.to_string())?;
        db.execute_batch("PRAGMA foreign_keys=ON;CREATE TABLE IF NOT EXISTS providers(id TEXT PRIMARY KEY,name TEXT NOT NULL,base_url TEXT NOT NULL,models_json TEXT NOT NULL);CREATE TABLE IF NOT EXISTS settings(singleton INTEGER PRIMARY KEY,json TEXT NOT NULL);").map_err(|e|e.to_string())?;
        connections::initialize(&db)?;
        Ok(db)
    }
    fn key(&self, id: &str) -> Result<credential_store::Entry, String> {
        credential_store::Entry::new(self.db_path.parent().unwrap().join("credentials"), id)
            .map_err(|e| e.to_string())
    }
}
fn endpoint(base: &str, path: &str) -> String {
    format!("{}/{path}", base.trim_end_matches('/'))
}
fn generation_token(state: &AppState, id: &str) -> Result<CancellationToken, String> {
    let mut map = state.cancellations.lock().unwrap();
    if map.contains_key(id) {
        return Err("active operation".into());
    }
    let token = CancellationToken::new();
    map.insert(id.into(), token.clone());
    Ok(token)
}
fn provider(state: &AppState, id: &str) -> Result<Provider, String> {
    load_data(state)?
        .providers
        .into_iter()
        .find(|p| p.id == id)
        .ok_or("missing provider".into())
}
fn load_data(state: &AppState) -> Result<AppData, String> {
    let db = state.db()?;
    let settings: Option<String> = db
        .query_row("SELECT json FROM settings WHERE singleton=1", [], |row| {
            row.get(0)
        })
        .optional()
        .map_err(|e| e.to_string())?;
    let settings = settings
        .map(|raw| serde_json::from_str(&raw).unwrap())
        .unwrap_or_default();
    let mut query = db
        .prepare("SELECT id,name,base_url,models_json FROM providers ORDER BY rowid")
        .unwrap();
    let providers = query
        .query_map([], |row| {
            let models: String = row.get(3)?;
            Ok(Provider {
                id: row.get(0)?,
                name: row.get(1)?,
                base_url: row.get(2)?,
                models: serde_json::from_str(&models).unwrap(),
            })
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    Ok(AppData {
        providers,
        settings,
    })
}

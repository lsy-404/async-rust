use crate::{credential_store, AppState};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Credential {
    pub id: String,
    pub provider_id: String,
    pub auth_method: String,
    pub label: String,
    pub account: Option<String>,
    pub enabled: bool,
    pub healthy: bool,
    pub weight: u32,
    pub models: Vec<String>,
    pub cooldown_until: Option<i64>,
}
impl Credential {
    pub fn new(provider_id: &str, auth_method: &str, label: String, models: Vec<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            provider_id: provider_id.into(),
            auth_method: auth_method.into(),
            label,
            account: None,
            enabled: true,
            healthy: true,
            weight: 1,
            models,
            cooldown_until: None,
        }
    }
    pub fn view(&self) -> Value {
        let mut value = json!({"id":self.id,"label":self.label,"enabled":self.enabled,"healthy":self.healthy,"weight":self.weight,"models":self.models,
            "cooldownUntilUtc":self.cooldown_until.filter(|value| *value > now()).and_then(chrono::DateTime::from_timestamp_millis).map(|time| time.to_rfc3339())});
        if let Some(account) = &self.account {
            value["account"] = json!(account);
        }
        value
    }
}
#[derive(Clone, Debug)]
pub struct Preferences {
    pub oauth_enabled: bool,
    pub strategy: String,
}
#[derive(Clone, Copy)]
pub enum Failure {
    Authentication,
    Temporary,
}
fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn storage(error: rusqlite::Error) -> String {
    format!("连接元数据读写失败：{error}")
}

pub fn initialize(db: &Connection) -> Result<(), String> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS credentials(
        id TEXT PRIMARY KEY, provider_id TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
        auth_method TEXT NOT NULL CHECK(auth_method IN ('oauth','api-key')),
        label TEXT NOT NULL, account TEXT, enabled INTEGER NOT NULL DEFAULT 1,
        healthy INTEGER NOT NULL DEFAULT 1, weight INTEGER NOT NULL DEFAULT 1 CHECK(weight BETWEEN 1 AND 100),
        models_json TEXT NOT NULL DEFAULT '[]', cooldown_until INTEGER, balance INTEGER NOT NULL DEFAULT 0);
        CREATE INDEX IF NOT EXISTS credentials_provider ON credentials(provider_id);
        CREATE TABLE IF NOT EXISTS provider_auth(
        provider_id TEXT PRIMARY KEY REFERENCES providers(id) ON DELETE CASCADE,
        oauth_enabled INTEGER NOT NULL DEFAULT 1,
        strategy TEXT NOT NULL DEFAULT 'round-robin' CHECK(strategy IN ('round-robin','weighted-round-robin','failover')),
        cursor INTEGER NOT NULL DEFAULT 0, catalog_error TEXT, checked_at INTEGER);").map_err(storage)
}
fn require_provider(db: &Connection, provider_id: &str) -> Result<(), String> {
    if !db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM providers WHERE id=?)",
            [provider_id],
            |row| row.get::<_, bool>(0),
        )
        .map_err(storage)?
    {
        return Err("找不到供应商。".into());
    }
    Ok(())
}
fn row_credential(row: &rusqlite::Row<'_>) -> rusqlite::Result<Credential> {
    let models: String = row.get(9)?;
    let models = serde_json::from_str(&models).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(9, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(Credential {
        id: row.get(0)?,
        provider_id: row.get(1)?,
        auth_method: row.get(2)?,
        label: row.get(3)?,
        account: row.get(4)?,
        enabled: row.get(5)?,
        healthy: row.get(6)?,
        weight: row.get(7)?,
        cooldown_until: row.get(8)?,
        models,
    })
}
fn list_db(db: &Connection, provider_id: &str) -> Result<Vec<Credential>, String> {
    let mut statement = db.prepare("SELECT id,provider_id,auth_method,label,account,enabled,healthy,weight,cooldown_until,models_json FROM credentials WHERE provider_id=? ORDER BY rowid").map_err(storage)?;
    let rows = statement
        .query_map([provider_id], row_credential)
        .map_err(storage)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(storage)
}
pub fn list(state: &AppState, provider_id: &str) -> Result<Vec<Credential>, String> {
    list_db(&state.db()?, provider_id)
}
pub fn get(state: &AppState, provider_id: &str, credential_id: &str) -> Result<Credential, String> {
    list(state, provider_id)?
        .into_iter()
        .find(|item| item.id == credential_id)
        .ok_or_else(|| "凭据不存在或不属于此供应商。".into())
}
pub fn has_credential(state: &AppState, provider_id: &str) -> Result<bool, String> {
    has_credential_db(&state.db()?, provider_id)
}
pub fn has_credential_db(db: &Connection, provider_id: &str) -> Result<bool, String> {
    let prefs = preferences_db(db, provider_id)?;
    Ok(list_db(db, provider_id)?.iter().any(|item| {
        item.enabled && item.healthy && (item.auth_method != "oauth" || prefs.oauth_enabled)
    }))
}
pub fn catalog_result(state: &AppState, provider_id: &str, failed: usize) -> Result<(), String> {
    let error = if failed > 0 {
        Some(format!("{failed} 个凭据无法刷新模型目录，已保留原模型。"))
    } else {
        None
    };
    state.db()?.execute("INSERT INTO provider_auth(provider_id,catalog_error,checked_at) VALUES(?,?,?) ON CONFLICT(provider_id) DO UPDATE SET catalog_error=excluded.catalog_error,checked_at=excluded.checked_at",params![provider_id,error,now()]).map_err(storage)?;
    Ok(())
}
pub fn catalog_status(state: &AppState) -> Result<Value, String> {
    let db = state.db()?;
    let (error, checked): (Option<String>, Option<i64>) = db
        .query_row(
            "SELECT MAX(catalog_error),MAX(checked_at) FROM provider_auth",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(storage)?;
    let mut value =
        json!({"state":if error.is_some(){"error"}else{"ready"},"source":"cached","error":error});
    if let Some(time) = checked.and_then(chrono::DateTime::from_timestamp_millis) {
        value["checkedAt"] = json!(time.to_rfc3339());
    }
    Ok(value)
}
fn preferences_db(db: &Connection, provider_id: &str) -> Result<Preferences, String> {
    db.query_row(
        "SELECT oauth_enabled,strategy FROM provider_auth WHERE provider_id=?",
        [provider_id],
        |row| {
            Ok(Preferences {
                oauth_enabled: row.get(0)?,
                strategy: row.get(1)?,
            })
        },
    )
    .optional()
    .map_err(storage)
    .map(|value| {
        value.unwrap_or(Preferences {
            oauth_enabled: true,
            strategy: "round-robin".into(),
        })
    })
}
pub fn preferences(state: &AppState, provider_id: &str) -> Result<Preferences, String> {
    preferences_db(&state.db()?, provider_id)
}
fn sync_models(db: &Connection, provider_id: &str) -> Result<Vec<String>, String> {
    let mut models: Vec<_> = list_db(db, provider_id)?
        .into_iter()
        .flat_map(|item| item.models)
        .collect();
    models.sort();
    models.dedup();
    Ok(models)
}
fn previous_secret(state: &AppState, id: &str) -> Result<Option<String>, String> {
    match state.key(id)?.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(credential_store::Error::NoEntry) => Ok(None),
        Err(_) => Err("无法读取本地凭据文件。".into()),
    }
}
fn restore_secret(state: &AppState, id: &str, previous: Option<String>) {
    if let Ok(entry) = state.key(id) {
        if let Some(value) = previous {
            let _ = entry.set_password(&value);
        } else {
            let _ = entry.delete_credential();
        }
    }
}
pub fn save(state: &AppState, item: &Credential, secret: &str) -> Result<(), String> {
    if item.id.is_empty()
        || item.label.trim().is_empty()
        || item.label.len() > 200
        || secret.trim().is_empty()
        || !(1..=100).contains(&item.weight)
    {
        return Err("凭据名称、内容或权重无效。".into());
    }
    let mut db = state.db()?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage)?;
    require_provider(&tx, &item.provider_id)?;
    let owner: Option<(String, String)> = tx
        .query_row(
            "SELECT provider_id,auth_method FROM credentials WHERE id=?",
            [&item.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(storage)?;
    if owner.is_some_and(|(provider, method)| {
        provider != item.provider_id || method != item.auth_method
    }) {
        return Err("凭据不属于此供应商或授权方式。".into());
    }
    tx.execute("INSERT INTO credentials(id,provider_id,auth_method,label,account,enabled,healthy,weight,models_json,cooldown_until) VALUES(?,?,?,?,?,?,?,?,?,?)
        ON CONFLICT(id) DO UPDATE SET label=excluded.label,account=excluded.account,enabled=excluded.enabled,healthy=excluded.healthy,weight=excluded.weight,models_json=excluded.models_json,cooldown_until=excluded.cooldown_until",
        params![item.id,item.provider_id,item.auth_method,item.label,item.account,item.enabled,item.healthy,item.weight,serde_json::to_string(&item.models).map_err(|_|"模型编码失败。")?,item.cooldown_until]).map_err(storage)?;
    sync_models(&tx, &item.provider_id)?;
    let previous = previous_secret(state, &item.id)?;
    state
        .key(&item.id)?
        .set_password(secret)
        .map_err(|_| "无法保存本地凭据文件。")?;
    if let Err(error) = tx.commit() {
        restore_secret(state, &item.id, previous);
        return Err(storage(error));
    }
    Ok(())
}
pub fn replace_secret(
    state: &AppState,
    provider_id: &str,
    credential_id: &str,
    secret: &str,
) -> Result<(), String> {
    let mut db = state.db()?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage)?;
    let item = list_db(&tx, provider_id)?
        .into_iter()
        .find(|item| item.id == credential_id)
        .ok_or("凭据已删除。")?;
    if !item.enabled {
        return Err("此凭据已停用。".into());
    }
    let previous = previous_secret(state, credential_id)?;
    state
        .key(credential_id)?
        .set_password(secret)
        .map_err(|_| "无法保存刷新后的凭据。")?;
    if let Err(error) = tx.commit() {
        restore_secret(state, credential_id, previous);
        return Err(storage(error));
    }
    Ok(())
}
pub fn remove(
    state: &AppState,
    provider_id: &str,
    credential_id: &str,
    auth_method: &str,
) -> Result<(), String> {
    let item = get(state, provider_id, credential_id)?;
    if item.auth_method != auth_method {
        return Err("凭据授权方式不匹配。".into());
    }
    let mut db = state.db()?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage)?;
    let previous = previous_secret(state, credential_id)?;
    tx.execute(
        "DELETE FROM credentials WHERE id=? AND provider_id=?",
        params![credential_id, provider_id],
    )
    .map_err(storage)?;
    sync_models(&tx, provider_id)?;
    match state.key(credential_id)?.delete_credential() {
        Ok(()) | Err(credential_store::Error::NoEntry) => {}
        Err(_) => return Err("无法删除本地凭据文件。".into()),
    }
    if let Err(error) = tx.commit() {
        restore_secret(state, credential_id, previous);
        return Err(storage(error));
    }
    Ok(())
}
pub fn update(
    state: &AppState,
    provider_id: &str,
    credential_id: &str,
    enabled: bool,
    weight: u32,
) -> Result<(), String> {
    if !(1..=100).contains(&weight) {
        return Err("凭据权重必须是 1 到 100 的整数。".into());
    }
    get(state, provider_id, credential_id)?;
    let db = state.db()?;
    db.execute(
        "UPDATE credentials SET enabled=?,weight=?,balance=0 WHERE id=? AND provider_id=?",
        params![enabled, weight, credential_id, provider_id],
    )
    .map_err(storage)?;
    Ok(())
}
pub fn set_oauth_enabled(state: &AppState, provider_id: &str, enabled: bool) -> Result<(), String> {
    let db = state.db()?;
    require_provider(&db, provider_id)?;
    db.execute("INSERT INTO provider_auth(provider_id,oauth_enabled) VALUES(?,?) ON CONFLICT(provider_id) DO UPDATE SET oauth_enabled=excluded.oauth_enabled",params![provider_id,enabled]).map_err(storage)?;
    Ok(())
}
pub fn set_strategy(state: &AppState, provider_id: &str, strategy: &str) -> Result<(), String> {
    if !matches!(
        strategy,
        "round-robin" | "weighted-round-robin" | "failover"
    ) {
        return Err("无效的负载策略。".into());
    }
    let mut db = state.db()?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage)?;
    require_provider(&tx, provider_id)?;
    tx.execute("INSERT INTO provider_auth(provider_id,strategy) VALUES(?,?) ON CONFLICT(provider_id) DO UPDATE SET strategy=excluded.strategy,cursor=0",params![provider_id,strategy]).map_err(storage)?;
    tx.execute(
        "UPDATE credentials SET balance=0 WHERE provider_id=?",
        [provider_id],
    )
    .map_err(storage)?;
    tx.commit().map_err(storage)
}
pub fn candidates(
    state: &AppState,
    provider_id: &str,
    model: &str,
) -> Result<Vec<Credential>, String> {
    let mut db = state.db()?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage)?;
    require_provider(&tx, provider_id)?;
    let prefs = preferences_db(&tx, provider_id)?;
    let mut items: Vec<_> = list_db(&tx, provider_id)?
        .into_iter()
        .filter(|item| {
            item.enabled
                && item.healthy
                && item.cooldown_until.is_none_or(|until| until <= now())
                && (item.auth_method != "oauth" || prefs.oauth_enabled)
                && item.models.iter().any(|name| name == model)
        })
        .collect();
    if items.is_empty() {
        return Ok(items);
    }
    match prefs.strategy.as_str() {
        "round-robin" => {
            tx.execute(
                "INSERT OR IGNORE INTO provider_auth(provider_id) VALUES(?)",
                [provider_id],
            )
            .map_err(storage)?;
            let cursor: u64 = tx
                .query_row(
                    "SELECT cursor FROM provider_auth WHERE provider_id=?",
                    [provider_id],
                    |row| row.get(0),
                )
                .map_err(storage)?;
            let index = (cursor % items.len() as u64) as usize;
            items.rotate_left(index);
            tx.execute(
                "UPDATE provider_auth SET cursor=? WHERE provider_id=?",
                params![(cursor + 1) % items.len() as u64, provider_id],
            )
            .map_err(storage)?;
        }
        "weighted-round-robin" => {
            let total: i64 = items.iter().map(|item| i64::from(item.weight)).sum();
            let mut ranked = Vec::new();
            for (index, item) in items.iter().enumerate() {
                tx.execute(
                    "UPDATE credentials SET balance=balance+? WHERE id=?",
                    params![item.weight, item.id],
                )
                .map_err(storage)?;
                let score: i64 = tx
                    .query_row(
                        "SELECT balance FROM credentials WHERE id=?",
                        [&item.id],
                        |row| row.get(0),
                    )
                    .map_err(storage)?;
                ranked.push((index, score));
            }
            ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            let selected = &items[ranked[0].0];
            tx.execute(
                "UPDATE credentials SET balance=balance-? WHERE id=?",
                params![total, selected.id],
            )
            .map_err(storage)?;
            items = ranked
                .into_iter()
                .map(|(index, _)| items[index].clone())
                .collect();
        }
        "failover" => {}
        _ => return Err("无效的负载策略。".into()),
    }
    tx.commit().map_err(storage)?;
    Ok(items)
}
pub fn report_success(
    state: &AppState,
    provider_id: &str,
    credential_id: &str,
) -> Result<(), String> {
    get(state, provider_id, credential_id)?;
    state
        .db()?
        .execute(
            "UPDATE credentials SET healthy=1,cooldown_until=NULL WHERE id=? AND provider_id=?",
            params![credential_id, provider_id],
        )
        .map_err(storage)?;
    Ok(())
}
pub fn report_error(
    state: &AppState,
    provider_id: &str,
    credential_id: &str,
    failure: Failure,
) -> Result<(), String> {
    get(state, provider_id, credential_id)?;
    let (healthy, cooldown) = match failure {
        Failure::Authentication => (false, None),
        Failure::Temporary => (true, Some(now() + 30_000)),
    };
    state
        .db()?
        .execute(
            "UPDATE credentials SET healthy=?,cooldown_until=? WHERE id=? AND provider_id=?",
            params![healthy, cooldown, credential_id, provider_id],
        )
        .map_err(storage)?;
    Ok(())
}
pub fn record_models(
    state: &AppState,
    provider_id: &str,
    credential_id: &str,
    models: &[String],
) -> Result<(), String> {
    get(state, provider_id, credential_id)?;
    let mut db = state.db()?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage)?;
    tx.execute("UPDATE credentials SET models_json=?,healthy=1,cooldown_until=NULL WHERE id=? AND provider_id=?",params![serde_json::to_string(models).map_err(|_|"模型编码失败。")?,credential_id,provider_id]).map_err(storage)?;
    sync_models(&tx, provider_id)?;
    tx.commit().map_err(storage)
}
#[cfg(test)]
#[path = "../../test/connections.rs"]
mod tests;

use crate::{generation_token, oauth, provider, AppState, Provider, StreamEvent};
use rusqlite::params;
use serde::Serialize;
use serde_json::Value;
use tauri::{ipc::Channel, State};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Serialize)]
pub struct AuthEvent {
    #[serde(rename = "type")]
    kind: String,
    text: String,
}

#[tauri::command]
pub async fn authorize_oauth(
    provider_id: String,
    on_event: Channel<AuthEvent>,
    state: State<'_, AppState>,
) -> Result<Provider, String> {
    if !oauth::is_oauth(&provider_id) {
        return Err("该供应商不支持 OAuth。".into());
    }
    let operation = format!("oauth:{provider_id}");
    let cancel = generation_token(&state, &operation)?;
    let progress = on_event.clone();
    let result = oauth::authorize(&provider_id, cancel.clone(), move |address| {
        let url = url::Url::parse(&address).map_err(|_| "授权地址无效。")?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err("授权地址必须是安全的 HTTPS 地址。".into());
        }
        open::that_detached(url.as_str())
            .map_err(|_| "无法打开系统浏览器，请检查默认浏览器设置。")?;
        progress
            .send(AuthEvent {
                kind: "status".into(),
                text: "请在系统浏览器中完成供应商授权。".into(),
            })
            .map_err(|e| e.to_string())
    })
    .await;
    state
        .cancellations
        .lock()
        .map_err(|_| "授权状态不可用。")?
        .remove(&operation);
    let credential = result?;
    if cancel.is_cancelled() {
        return Err("授权已取消。".into());
    }
    state
        .key(&provider_id)?
        .set_password(&serde_json::to_string(&credential).map_err(|e| e.to_string())?)
        .map_err(|_| "无法把供应商凭据保存到系统凭据库。")?;
    match oauth::models(&provider_id, &credential).await {
        Ok(models) => store_models(&state, &provider_id, &models)?,
        Err(_) => {
            on_event
                .send(AuthEvent {
                    kind: "status".into(),
                    text: "授权已保存。模型目录暂时不可用，可稍后刷新。".into(),
                })
                .map_err(|e| e.to_string())?;
        }
    }
    provider(&state, &provider_id)
}

#[tauri::command]
pub fn cancel_oauth(provider_id: String, state: State<'_, AppState>) -> Result<(), String> {
    if let Some(token) = state
        .cancellations
        .lock()
        .map_err(|_| "授权状态不可用。")?
        .get(&format!("oauth:{provider_id}"))
    {
        token.cancel();
    }
    Ok(())
}

#[tauri::command]
pub fn remove_oauth(provider_id: String, state: State<'_, AppState>) -> Result<(), String> {
    if !oauth::is_oauth(&provider_id) {
        return Err("该供应商不支持 OAuth。".into());
    }
    cancel_oauth(provider_id.clone(), state.clone())?;
    match state.key(&provider_id)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(_) => Err("无法从系统凭据库删除授权。".into()),
    }
}

async fn credential(
    state: &AppState,
    id: &str,
    cancel: CancellationToken,
) -> Result<Value, String> {
    let raw = state.api_key(id)?;
    let saved: Value = serde_json::from_str(&raw).map_err(|_| "供应商凭据无效，请重新授权。")?;
    if !oauth::needs_refresh(id, &saved) {
        return Ok(saved);
    }
    let renewed = oauth::refresh(id, &saved, cancel).await?;
    state
        .key(id)?
        .set_password(&serde_json::to_string(&renewed).map_err(|e| e.to_string())?)
        .map_err(|_| "无法保存刷新后的供应商凭据。")?;
    Ok(renewed)
}

fn store_models(state: &AppState, id: &str, models: &[String]) -> Result<(), String> {
    state
        .db()?
        .execute(
            "UPDATE providers SET models_json=? WHERE id=?",
            params![
                serde_json::to_string(models).map_err(|e| e.to_string())?,
                id
            ],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub async fn models(state: &AppState, id: &str) -> Result<Vec<String>, String> {
    let saved = credential(state, id, CancellationToken::new()).await?;
    let result = oauth::models(id, &saved).await?;
    store_models(state, id, &result)?;
    Ok(result)
}

pub async fn stream(
    state: &AppState,
    id: &str,
    model: &str,
    messages: Vec<Value>,
    cancel: CancellationToken,
    channel: Channel<StreamEvent>,
) -> Result<String, String> {
    let saved = credential(state, id, cancel.clone()).await?;
    let output = channel.clone();
    let answer = oauth::chat(id, &saved, model, messages, cancel, move |text| {
        output
            .send(StreamEvent {
                kind: "delta".into(),
                text,
            })
            .map_err(|e| e.to_string())
    })
    .await?;
    channel
        .send(StreamEvent {
            kind: "done".into(),
            text: String::new(),
        })
        .map_err(|e| e.to_string())?;
    Ok(answer)
}

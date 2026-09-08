use crate::{
    connections::{self, Credential, Failure},
    generation_token, load_data, oauth, provider, AppState, Provider, StreamEvent,
};
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tauri::{ipc::Channel, State};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Serialize)]
pub struct AuthEvent {
    #[serde(rename = "type")]
    pub kind: String,
    pub text: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ApiKeyPayload {
    provider_id: String,
    label: String,
    api_key: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CredentialPayload {
    provider_id: String,
    credential_id: String,
    enabled: bool,
    weight: u32,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderPayload {
    provider_id: String,
    oauth_enabled: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SelectionPayload {
    provider_id: String,
    model: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StrategyPayload {
    provider_id: String,
    strategy: String,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
enum Action {
    AuthorizeOauth {
        #[serde(rename = "providerId")]
        provider_id: String,
        #[serde(rename = "credentialId")]
        credential_id: Option<String>,
    },
    AddApiKey {
        payload: ApiKeyPayload,
    },
    RemoveCredential {
        #[serde(rename = "providerId")]
        provider_id: String,
        #[serde(rename = "credentialId")]
        credential_id: String,
        #[serde(rename = "authMethod")]
        auth_method: String,
    },
    UpdateCredential {
        payload: CredentialPayload,
    },
    UpdateProvider {
        payload: ProviderPayload,
    },
    SelectModel {
        payload: SelectionPayload,
    },
    UpdateStrategy {
        payload: StrategyPayload,
    },
    RefreshCatalog,
}
fn auth_state(state: &AppState) -> Result<Value, String> {
    let data = load_data(state)?;
    let mut providers = Vec::new();
    let mut unavailable_secrets = 0usize;
    for provider in data.providers {
        let prefs = connections::preferences(state, &provider.id)?;
        let mut credentials = connections::list(state, &provider.id)?;
        for item in &mut credentials {
            if !state
                .key(&item.id)?
                .get_password()
                .is_ok_and(|secret| !secret.trim().is_empty())
            {
                item.healthy = false;
                unavailable_secrets += 1;
            }
        }
        let oauth_credentials: Vec<_> = credentials
            .iter()
            .filter(|item| item.auth_method == "oauth")
            .map(Credential::view)
            .collect();
        let api_credentials: Vec<_> = credentials
            .iter()
            .filter(|item| item.auth_method == "api-key")
            .map(Credential::view)
            .collect();
        let method = if oauth::is_oauth(&provider.id) {
            "oauth"
        } else {
            "api-key"
        };
        let mut oauth_models = Vec::new();
        let mut api_models = Vec::new();
        for item in &credentials {
            if item.auth_method == "oauth" {
                oauth_models.extend(item.models.clone());
            } else {
                api_models.extend(item.models.clone());
            }
        }
        oauth_models.sort();
        oauth_models.dedup();
        api_models.sort();
        api_models.dedup();
        providers.push(json!({"id":provider.id,"name":provider.name,"description":if method=="oauth" {"在系统浏览器中授权，可添加多个账号。"}else{"使用供应商 API Key，可添加多个密钥。"},
            "authMethods":[method],"available":true,"oauthEnabled":prefs.oauth_enabled,"loadStrategy":prefs.strategy,"models":provider.models,
            "oauthModels":oauth_models,"apiKeyModels":api_models,"oauthCredentials":oauth_credentials,"apiKeyCredentials":api_credentials}));
    }
    let model = if data.settings.model.is_empty() {
        Value::Null
    } else {
        json!({"providerId":data.settings.provider_id,"model":data.settings.model})
    };
    let mut catalog = connections::catalog_status(state)?;
    if unavailable_secrets > 0 {
        catalog["state"] = json!("error");
        catalog["error"] = json!(format!(
            "{unavailable_secrets} 个本地凭据无法读取，请重新连接。"
        ));
    }
    Ok(json!({"providers":providers,"model":model,"catalogStatus":catalog}))
}
#[tauri::command]
pub fn get_auth_state(state: State<'_, AppState>) -> Result<Value, String> {
    auth_state(&state)
}

#[tauri::command]
pub async fn model_auth_action(
    action: Value,
    on_event: Channel<AuthEvent>,
    operation_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let action: Action = serde_json::from_value(action).map_err(|_| "授权操作参数无效。")?;
    let operation_id = operation_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    if uuid::Uuid::parse_str(&operation_id).is_err() {
        return Err("授权操作标识无效。".into());
    }
    let operation = format!("model-auth:{operation_id}");
    let cancel = generation_token(&state, &operation)?;
    let result = execute_action(&state, action, cancel, move |text| {
        on_event
            .send(AuthEvent {
                kind: "status".into(),
                text,
            })
            .map_err(|_| "授权状态通道已关闭。".into())
    })
    .await;
    state
        .cancellations
        .lock()
        .map_err(|_| "授权状态不可用。")?
        .remove(&operation);
    result
}
#[tauri::command]
pub fn cancel_model_auth(operation_id: String, state: State<'_, AppState>) -> Result<(), String> {
    if let Some(token) = state
        .cancellations
        .lock()
        .map_err(|_| "授权状态不可用。")?
        .get(&format!("model-auth:{operation_id}"))
    {
        token.cancel();
    }
    Ok(())
}
async fn execute_action(
    state: &AppState,
    action: Action,
    cancel: CancellationToken,
    on_status: impl Fn(String) -> Result<(), String> + Send + Sync,
) -> Result<(), String> {
    if cancel.is_cancelled() {
        return Err("授权操作已取消。".into());
    }
    match action {
        Action::AuthorizeOauth {
            provider_id,
            credential_id,
        } => {
            authorize(
                state,
                &provider_id,
                credential_id.as_deref(),
                cancel,
                on_status,
            )
            .await
        }
        Action::AddApiKey { payload } => {
            let p = provider(state, &payload.provider_id)?;
            if oauth::is_oauth(&p.id) {
                return Err("此供应商需要 OAuth 授权。".into());
            }
            let label = payload.label.trim();
            let key = payload.api_key.trim();
            if label.is_empty() || label.len() > 200 || key.is_empty() {
                return Err("请输入凭据名称和 API Key。".into());
            }
            let models = api_models(state, &p, key, cancel.clone()).await?;
            if cancel.is_cancelled() {
                return Err("授权操作已取消。".into());
            }
            let item = Credential::new(&p.id, "api-key", label.into(), models);
            connections::save(state, &item, key)
        }
        Action::RemoveCredential {
            provider_id,
            credential_id,
            auth_method,
        } => connections::remove(state, &provider_id, &credential_id, &auth_method),
        Action::UpdateCredential { payload } => connections::update(
            state,
            &payload.provider_id,
            &payload.credential_id,
            payload.enabled,
            payload.weight,
        ),
        Action::UpdateProvider { payload } => {
            if !oauth::is_oauth(&payload.provider_id) {
                return Err("此供应商不支持 OAuth 开关。".into());
            }
            connections::set_oauth_enabled(state, &payload.provider_id, payload.oauth_enabled)
        }
        Action::UpdateStrategy { payload } => {
            connections::set_strategy(state, &payload.provider_id, &payload.strategy)
        }
        Action::SelectModel { payload } => {
            let p = provider(state, &payload.provider_id)?;
            if payload.model.trim().is_empty() || !p.models.contains(&payload.model) {
                return Err("此供应商没有所选模型，请刷新模型目录。".into());
            }
            let mut settings = load_data(state)?.settings;
            settings.provider_id = payload.provider_id;
            settings.model = payload.model;
            state.db()?.execute("INSERT INTO settings(singleton,json) VALUES(1,?) ON CONFLICT(singleton) DO UPDATE SET json=excluded.json",[serde_json::to_string(&settings).map_err(|_|"设置编码失败。")?]).map_err(|e|e.to_string())?;
            Ok(())
        }
        Action::RefreshCatalog => {
            let catalog_error = crate::models_catalog::refresh(state).await.err();
            let providers = load_data(state)?.providers;
            let mut failures = 0usize;
            for p in providers {
                if cancel.is_cancelled() {
                    return Err("模型刷新已取消。".into());
                }
                let (_, failed) = discover(state, &p.id, cancel.clone()).await?;
                failures += failed;
            }
            if failures > 0 || catalog_error.is_some() {
                on_status(format!(
                    "models.dev 或 {failures} 个凭据未能刷新模型，已保留缓存目录并更新连接状态。"
                ))?;
            }
            Ok(())
        }
    }
}
async fn authorize(
    state: &AppState,
    provider_id: &str,
    credential_id: Option<&str>,
    cancel: CancellationToken,
    on_status: impl Fn(String) -> Result<(), String> + Send + Sync,
) -> Result<(), String> {
    provider(state, provider_id)?;
    if !oauth::is_oauth(provider_id) {
        return Err("此供应商不支持 OAuth。".into());
    }
    if !connections::preferences(state, provider_id)?.oauth_enabled {
        return Err("请先启用此供应商的 OAuth。".into());
    }
    let previous = credential_id
        .map(|id| connections::get(state, provider_id, id))
        .transpose()?;
    if previous
        .as_ref()
        .is_some_and(|item| item.auth_method != "oauth")
    {
        return Err("此凭据不是 OAuth 账号。".into());
    }
    let credential = oauth::authorize(provider_id, cancel.clone(), |address| {
        let url = url::Url::parse(&address).map_err(|_| "授权地址无效。")?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err("授权地址必须是 HTTPS 地址。".into());
        }
        open::that_detached(url.as_str()).map_err(|_| "无法打开系统浏览器。")?;
        on_status("请在系统浏览器中完成供应商授权。".into())
    })
    .await?;
    let account = credential
        .get("accountId")
        .or_else(|| credential.get("userId"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let label = credential
        .get("label")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| account.clone())
        .unwrap_or_else(|| "OAuth 账号".into());
    let mut item = previous
        .unwrap_or_else(|| Credential::new(provider_id, "oauth", label.clone(), Vec::new()));
    item.label = label;
    item.account = account;
    item.cooldown_until = None;
    let discovered = tokio::select! {_ = cancel.cancelled()=>return Err("授权操作已取消。".into()),result=oauth::models(provider_id,&credential)=>result};
    match discovered {
        Ok(models) => {
            item.models = models;
            item.healthy = true;
        }
        Err(_) => {
            item.healthy = false;
        }
    }
    if cancel.is_cancelled() {
        return Err("授权操作已取消。".into());
    }
    if let Some(id) = credential_id {
        connections::get(state, provider_id, id)?;
    }
    connections::save(
        state,
        &item,
        &serde_json::to_string(&credential).map_err(|_| "凭据编码失败。")?,
    )?;
    if !item.healthy {
        connections::catalog_result(state, provider_id, 1)?;
        on_status("授权已保存，但模型目录暂时不可用。请刷新目录后使用。".into())?;
    }
    Ok(())
}

pub async fn credential_for_request(
    state: &AppState,
    provider_id: &str,
    credential_id: &str,
    cancel: CancellationToken,
) -> Result<Value, String> {
    let item = connections::get(state, provider_id, credential_id)?;
    if !item.enabled {
        return Err("此凭据已停用。".into());
    }
    if item.auth_method == "oauth" && !connections::preferences(state, provider_id)?.oauth_enabled {
        return Err("此供应商的 OAuth 已停用。".into());
    }
    let read = || {
        state
            .key(credential_id)?
            .get_password()
            .map_err(|_| "无法读取此凭据，请重新连接。".to_owned())
    };
    if item.auth_method == "api-key" {
        return Ok(json!({"apiKey":read()?}));
    }
    static REFRESH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let raw = read()?;
    let saved: Value =
        serde_json::from_str(&raw).map_err(|_| "本地 OAuth 凭据无效，请重新连接。")?;
    if !oauth::needs_refresh(provider_id, &saved) {
        return Ok(saved);
    }
    let _guard = tokio::select! {_ = cancel.cancelled()=>return Err("授权操作已取消。".into()),guard=REFRESH_LOCK.lock()=>guard};
    let current: Value =
        serde_json::from_str(&read()?).map_err(|_| "本地 OAuth 凭据无效，请重新连接。")?;
    if !oauth::needs_refresh(provider_id, &current) {
        return Ok(current);
    }
    let refreshed = oauth::refresh(provider_id, &current, cancel.clone()).await?;
    if cancel.is_cancelled() {
        return Err("授权操作已取消。".into());
    }
    connections::replace_secret(
        state,
        provider_id,
        credential_id,
        &serde_json::to_string(&refreshed).map_err(|_| "凭据编码失败。")?,
    )?;
    Ok(refreshed)
}
async fn api_models(
    state: &AppState,
    p: &Provider,
    key: &str,
    cancel: CancellationToken,
) -> Result<Vec<String>, String> {
    let response = tokio::select! {_ = cancel.cancelled()=>return Err("模型刷新已取消。".into()),response=state.client.get(crate::endpoint(&p.base_url,"models")).bearer_auth(key).send()=>response.map_err(|_|"无法连接模型供应商。")?};
    if !response.status().is_success() {
        return Err(format!(
            "模型供应商拒绝请求（HTTP {}）。",
            response.status().as_u16()
        ));
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    loop {
        let part = tokio::select! {_ = cancel.cancelled()=>return Err("模型刷新已取消。".into()),part=stream.next()=>part};
        let Some(part) = part else { break };
        let part = part.map_err(|_| "模型目录响应已中断。")?;
        if bytes.len() + part.len() > 8 * 1024 * 1024 {
            return Err("模型目录响应过大。".into());
        }
        bytes.extend_from_slice(&part);
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| "模型目录格式无效。")?;
    let values = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or("模型目录缺少 data 数组。")?;
    let mut models = values
        .iter()
        .map(|item| {
            item.get("id")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned)
                .ok_or_else(|| "模型目录包含无效条目。".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    models.sort();
    models.dedup();
    if models.is_empty() {
        return Err("此凭据没有可用模型。".into());
    }
    Ok(models)
}
fn failure(error: &str) -> Failure {
    if error.contains("401")
        || error.contains("403")
        || error.contains("expired")
        || error.contains("重新连接")
        || error.contains("invalid credential")
    {
        Failure::Authentication
    } else {
        Failure::Temporary
    }
}
async fn discover(
    state: &AppState,
    provider_id: &str,
    cancel: CancellationToken,
) -> Result<(Vec<String>, usize), String> {
    let p = provider(state, provider_id)?;
    let prefs = connections::preferences(state, provider_id)?;
    let mut failed = 0usize;
    for item in connections::list(state, provider_id)? {
        if !item.enabled || (item.auth_method == "oauth" && !prefs.oauth_enabled) {
            continue;
        }
        let result=async {
            let saved=credential_for_request(state,provider_id,&item.id,cancel.clone()).await?;
            if item.auth_method=="oauth"{tokio::select!{_ = cancel.cancelled()=>Err("模型刷新已取消。".into()),result=oauth::models(provider_id,&saved)=>result}}
            else{api_models(state,&p,saved["apiKey"].as_str().ok_or("本地 API Key 无效。")?,cancel.clone()).await}
        }.await;
        if cancel.is_cancelled() {
            return Err("模型刷新已取消。".into());
        }
        match result {
            Ok(models) => connections::record_models(state, provider_id, &item.id, &models)?,
            Err(_) => {
                failed += 1;
                connections::report_error(state, provider_id, &item.id, Failure::Authentication)?;
            }
        }
    }
    connections::catalog_result(state, provider_id, failed)?;
    let mut models: Vec<_> = connections::list(state, provider_id)?
        .into_iter()
        .flat_map(|item| item.models)
        .collect();
    models.sort();
    models.dedup();
    Ok((models, failed))
}
pub async fn models(state: &AppState, provider_id: &str) -> Result<Vec<String>, String> {
    let (models, failures) = discover(state, provider_id, CancellationToken::new()).await?;
    if failures > 0 {
        return Err("部分凭据无法刷新目录，已保留原模型并更新连接状态。".into());
    }
    Ok(models)
}

pub async fn stream(
    state: &AppState,
    provider_id: &str,
    model: &str,
    messages: Vec<Value>,
    cancel: CancellationToken,
    channel: Channel<StreamEvent>,
) -> Result<String, String> {
    let answer = stream_with(state, provider_id, model, messages, cancel, |text| {
        channel
            .send(StreamEvent {
                kind: "delta".into(),
                text,
            })
            .map_err(|_| "聊天状态通道已关闭。".into())
    })
    .await?;
    channel
        .send(StreamEvent {
            kind: "done".into(),
            text: String::new(),
        })
        .map_err(|_| "聊天状态通道已关闭。")?;
    Ok(answer)
}
async fn stream_with(
    state: &AppState,
    provider_id: &str,
    model: &str,
    messages: Vec<Value>,
    cancel: CancellationToken,
    on_delta: impl Fn(String) -> Result<(), String> + Send + Sync,
) -> Result<String, String> {
    let p = provider(state, provider_id)?;
    let candidates = connections::candidates(state, provider_id, model)?;
    if candidates.is_empty() {
        return Err("没有已启用、状态正常且支持所选模型的凭据。".into());
    }
    let mut last_error = "没有可用凭据。".to_owned();
    for item in candidates {
        if cancel.is_cancelled() {
            return Err("生成已取消。".into());
        }
        let emitted = Arc::new(AtomicBool::new(false));
        let sent = emitted.clone();
        let delta = |text: String| {
            if !text.is_empty() {
                sent.store(true, Ordering::Relaxed);
            }
            on_delta(text)
        };
        let result = async {
            let saved =
                credential_for_request(state, provider_id, &item.id, cancel.clone()).await?;
            if item.auth_method == "oauth" {
                oauth::chat(
                    provider_id,
                    &saved,
                    model,
                    messages.clone(),
                    cancel.clone(),
                    delta,
                )
                .await
            } else {
                api_stream(
                    state,
                    &p,
                    saved["apiKey"].as_str().ok_or("本地 API Key 无效。")?,
                    model,
                    &messages,
                    cancel.clone(),
                    delta,
                )
                .await
            }
        }
        .await;
        if cancel.is_cancelled() {
            return Err("生成已取消。".into());
        }
        match result {
            Ok(answer) => {
                connections::report_success(state, provider_id, &item.id)?;
                return Ok(answer);
            }
            Err(error) => {
                connections::report_error(state, provider_id, &item.id, failure(&error))?;
                if emitted.load(Ordering::Relaxed) {
                    return Err(error);
                }
                last_error = error;
            }
        }
    }
    Err(last_error)
}
async fn api_stream(
    state: &AppState,
    p: &Provider,
    key: &str,
    model: &str,
    messages: &[Value],
    cancel: CancellationToken,
    on_delta: impl Fn(String) -> Result<(), String> + Send,
) -> Result<String, String> {
    let response = tokio::select! {_ = cancel.cancelled()=>return Err("生成已取消。".into()),response=state.client.post(crate::endpoint(&p.base_url,"chat/completions")).bearer_auth(key).json(&json!({"model":model,"messages":messages,"stream":true})).send()=>response.map_err(|_|"无法连接模型供应商。")?};
    if !response.status().is_success() {
        return Err(format!(
            "模型供应商拒绝请求（HTTP {}）。",
            response.status().as_u16()
        ));
    }
    if !response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().starts_with("text/event-stream"))
    {
        return Err("模型供应商未返回流式响应。".into());
    }
    let mut received = 0usize;
    let mut stream = response
        .bytes_stream()
        .map(move |part| {
            let bytes = part.map_err(|_| "模型响应中断。".to_owned())?;
            received += bytes.len();
            if received > 16 * 1024 * 1024 {
                return Err("模型响应过大。".to_owned());
            }
            Ok(bytes)
        })
        .eventsource();
    let mut answer = String::new();
    loop {
        let next = tokio::select! {_ = cancel.cancelled()=>return Err("生成已取消。".into()),next=stream.next()=>next};
        let Some(next) = next else {
            return Err("模型响应在完成标记前中断。".into());
        };
        let event = next.map_err(|_| "模型响应格式无效。")?;
        if event.data == "[DONE]" {
            return Ok(answer);
        }
        let value: Value =
            serde_json::from_str(&event.data).map_err(|_| "模型响应包含无效 JSON。")?;
        if event.event == "error" || value.get("error").is_some() {
            return Err("模型供应商返回错误。".into());
        }
        let choices = value["choices"]
            .as_array()
            .ok_or("模型响应缺少 choices。")?;
        for choice in choices {
            if choice["index"].as_u64().unwrap_or(0) != 0 {
                continue;
            }
            if choice["delta"].get("tool_calls").is_some()
                || choice["delta"].get("function_call").is_some()
            {
                return Err("当前对话不支持工具调用响应。".into());
            }
            if let Some(value) = choice["delta"].get("content").filter(|v| !v.is_null()) {
                let text = value.as_str().ok_or("模型响应文本无效。")?;
                answer.push_str(text);
                on_delta(text.into())?;
            }
        }
    }
}
#[cfg(test)]
#[path = "../../test/model_auth_host.rs"]
mod tests;

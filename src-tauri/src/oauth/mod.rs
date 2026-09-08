use serde_json::Value;
use std::{future::Future, time::Duration};
use tokio_util::sync::CancellationToken;
mod trae;
mod workbuddy;
type Result<T> = std::result::Result<T, String>;

pub fn is_oauth(provider_id: &str) -> bool {
    matches!(provider_id, "workbuddy" | "traecode")
}

pub fn needs_refresh(provider_id: &str, credential: &Value) -> bool {
    let field = if provider_id == "traecode" {
        "expires"
    } else {
        "expires_at"
    };
    credential.get(field).and_then(Value::as_i64).unwrap_or(0)
        <= chrono::Utc::now().timestamp_millis() + 60_000
}

async fn cancellable<T>(
    cancel: CancellationToken,
    seconds: u64,
    future: impl Future<Output = Result<T>>,
) -> Result<T> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err("Operation cancelled.".into()),
        result = tokio::time::timeout(Duration::from_secs(seconds), future) => result.map_err(|_| "Operation timed out.".to_owned())?,
    }
}

pub async fn authorize(
    provider_id: &str,
    cancel: CancellationToken,
    on_url: impl Fn(String) -> Result<()> + Send,
) -> Result<Value> {
    cancellable(cancel, 600, async {
        match provider_id {
            "workbuddy" => workbuddy::authorize(on_url).await,
            "traecode" => trae::authorize(on_url).await,
            _ => Err("Unsupported authorization provider.".into()),
        }
    })
    .await
}

pub async fn refresh(
    provider_id: &str,
    credential: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    cancellable(cancel, 60, async {
        match provider_id {
            "workbuddy" => workbuddy::refresh(credential).await,
            "traecode" => trae::refresh_credential(credential).await,
            _ => Err("Unsupported authorization provider.".into()),
        }
    })
    .await
}

pub async fn models(provider_id: &str, credential: &Value) -> Result<Vec<String>> {
    match provider_id {
        "workbuddy" => workbuddy::models(credential),
        "traecode" => trae::list_models(credential).await,
        _ => Err("Unsupported authorization provider.".into()),
    }
}

pub async fn chat(
    provider_id: &str,
    credential: &Value,
    model: &str,
    messages: Vec<Value>,
    cancel: CancellationToken,
    on_delta: impl Fn(String) -> Result<()> + Send,
) -> Result<String> {
    if model.trim().is_empty() {
        return Err("Select a model.".into());
    }
    cancellable(cancel, 300, async {
        match provider_id {
            "workbuddy" => workbuddy::chat(credential, model, messages, on_delta).await,
            "traecode" => trae::chat(credential, model, messages, on_delta).await,
            _ => Err("Unsupported authorization provider.".into()),
        }
    })
    .await
}

fn validate_messages(messages: &[Value]) -> Result<Vec<Value>> {
    if messages.is_empty() {
        return Err("At least one message is required.".into());
    }
    messages
        .iter()
        .map(|message| {
            let role = message
                .get("role")
                .and_then(Value::as_str)
                .filter(|r| matches!(*r, "system" | "user" | "assistant"))
                .ok_or_else(|| "Unsupported message role.".to_owned())?;
            let content = message
                .get("content")
                .and_then(Value::as_str)
                .ok_or_else(|| "Message content must be text.".to_owned())?;
            if message.get("tool_calls").is_some() || message.get("tool_call_id").is_some() {
                return Err("Tool messages are not supported.".into());
            }
            Ok(serde_json::json!({"role":role,"content":content}))
        })
        .collect()
}

fn require_sse(response: &reqwest::Response) -> Result<()> {
    if !response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().starts_with("text/event-stream"))
    {
        return Err("Provider returned a non-streaming response.".into());
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../../test/oauth_runtime.rs"]
mod tests;

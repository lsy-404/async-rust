use super::Result;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use reqwest::{Client, Method, RequestBuilder, Response};
use serde_json::{json, Value};
use std::time::Duration;
use url::Url;

const API_BASE: &str = "https://copilot.tencent.com/v2/plugin";
const CHAT_URL: &str = "https://copilot.tencent.com/v2/chat/completions";
const ORIGIN: &str = "https://www.codebuddy.cn";
const MODELS: &[&str] = &[
    "glm-5.2",
    "glm-5.1",
    "glm-5v-turbo",
    "kimi-k2.7",
    "minimax-m3-pay",
    "hy3",
    "deepseek-v4-pro",
    "deepseek-v4-flash",
];
const MAX_RESPONSE: usize = 8 * 1024 * 1024;

fn client() -> Result<Client> {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|_| "WorkBuddy HTTP client unavailable.".into())
}
fn request(client: &Client, method: Method, url: &str) -> RequestBuilder {
    client
        .request(method, url)
        .header("accept", "application/json, text/plain, */*")
        .header("content-type", "application/json")
        .header("x-requested-with", "XMLHttpRequest")
        .header("origin", ORIGIN)
        .header("referer", format!("{ORIGIN}/"))
        .header("user-agent", "CLI/2.63.2 CodeBuddy/2.63.2")
        .header("x-product", "SaaS")
}
fn credential_headers(mut request: RequestBuilder, credential: &Value) -> Result<RequestBuilder> {
    let access = required(credential, &["access"])?;
    let refresh = required(credential, &["refresh"])?;
    request = request
        .bearer_auth(access)
        .header("x-refresh-token", refresh);
    for (field, header) in [
        ("domain", "x-domain"),
        ("userId", "x-user-id"),
        ("enterpriseId", "x-enterprise-id"),
    ] {
        if let Some(value) = credential[field].as_str().filter(|s| !s.trim().is_empty()) {
            request = request.header(header, value);
        }
    }
    Ok(request)
}
async fn send(request: RequestBuilder) -> Result<Response> {
    let response = request
        .send()
        .await
        .map_err(|_| "WorkBuddy service request failed.".to_owned())?;
    if !response.status().is_success() {
        return Err(format!(
            "WorkBuddy request rejected (HTTP {}).",
            response.status().as_u16()
        ));
    }
    Ok(response)
}
async fn response_json(response: Response) -> Result<Value> {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(part) = stream.next().await {
        let part = part.map_err(|_| "WorkBuddy response interrupted.".to_owned())?;
        if bytes.len() + part.len() > MAX_RESPONSE {
            return Err("WorkBuddy response exceeds size limit.".into());
        }
        bytes.extend_from_slice(&part);
    }
    serde_json::from_slice(&bytes).map_err(|_| "WorkBuddy returned invalid JSON.".into())
}
fn data(value: Value) -> Result<Value> {
    if let Some(code) = value.get("code") {
        if code.as_i64() != Some(0) {
            return Err("WorkBuddy request was rejected.".into());
        }
    }
    Ok(value.get("data").cloned().unwrap_or(value))
}
fn required(value: &Value, names: &[&str]) -> Result<String> {
    names
        .iter()
        .find_map(|name| {
            value[*name]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .map(str::to_owned)
        .ok_or_else(|| "WorkBuddy returned incomplete credentials.".to_owned())
}
fn integer(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| value.as_str()?.parse().ok())
}
fn credential(value: Value) -> Result<Value> {
    let access = required(&value, &["accessToken", "access_token", "access"])?;
    let refresh = required(&value, &["refreshToken", "refresh_token", "refresh"])?;
    let now = chrono::Utc::now().timestamp_millis();
    let expiry = value
        .get("expiresAt")
        .or_else(|| value.get("expires_at"))
        .and_then(integer)
        .or_else(|| {
            value
                .get("expiresIn")
                .or_else(|| value.get("expires_in"))
                .and_then(integer)
                .map(|seconds| {
                    now.saturating_add(seconds.saturating_mul(1000))
                        .saturating_sub(300_000)
                })
        })
        .filter(|expiry| *expiry > now)
        .ok_or_else(|| "WorkBuddy returned an invalid credential expiry.".to_owned())?;
    Ok(
        json!({"access":access,"refresh":refresh,"expires_at":expiry,"domain":value["domain"],"userId":value.get("userId").or_else(|| value.get("uid")),"enterpriseId":value["enterpriseId"]}),
    )
}
fn auth_state(value: Value) -> Result<(String, String)> {
    let value = data(value)?;
    let state = required(&value, &["state"])?;
    let auth_url = required(&value, &["authUrl", "auth_url"])?;
    let url =
        Url::parse(&auth_url).map_err(|_| "Invalid WorkBuddy authorization URL.".to_owned())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("Invalid WorkBuddy authorization URL.".into());
    }
    Ok((state, auth_url))
}
fn poll_result(value: Value) -> Result<Option<Value>> {
    if value.get("code").and_then(Value::as_i64) == Some(11217) {
        return Ok(None);
    }
    credential(data(value)?).map(Some)
}
pub(super) async fn authorize(on_url: impl Fn(String) -> Result<()> + Send) -> Result<Value> {
    authorize_at(API_BASE, Duration::from_millis(1500), on_url).await
}
async fn authorize_at(
    api_base: &str,
    interval: Duration,
    on_url: impl Fn(String) -> Result<()> + Send,
) -> Result<Value> {
    let client = client()?;
    let response = send(
        request(&client, Method::POST, &format!("{api_base}/auth/state"))
            .query(&[("platform", "workbuddy")])
            .json(&json!({})),
    )
    .await?;
    let (state, auth_url) = auth_state(response_json(response).await?)?;
    on_url(auth_url)?;
    for _ in 0..400 {
        let response = send(
            request(&client, Method::GET, &format!("{api_base}/auth/token"))
                .query(&[("state", &state)]),
        )
        .await?;
        if let Some(mut credential) = poll_result(response_json(response).await?)? {
            let response = send(credential_headers(
                request(&client, Method::GET, &format!("{api_base}/login/account"))
                    .query(&[("state", &state)]),
                &credential,
            )?)
            .await?;
            let account = data(response_json(response).await?)?;
            let user_id = required(&account, &["user_id", "uid", "userId"])?;
            credential["userId"] = json!(user_id);
            credential["label"] = json!(required(
                &account,
                &["display_name", "nickname", "displayName"]
            )
            .unwrap_or(user_id));
            if account["enterpriseId"].is_string() {
                credential["enterpriseId"] = account["enterpriseId"].clone();
            }
            return Ok(credential);
        }
        tokio::time::sleep(interval).await;
    }
    Err("WorkBuddy authorization timed out.".into())
}
pub(super) async fn refresh(value: &Value) -> Result<Value> {
    refresh_at(API_BASE, value).await
}
async fn refresh_at(api_base: &str, value: &Value) -> Result<Value> {
    let client = client()?;
    let response = send(
        credential_headers(
            request(
                &client,
                Method::POST,
                &format!("{api_base}/auth/token/refresh"),
            ),
            value,
        )?
        .header("x-auth-refresh-source", "workbuddy")
        .json(&json!({})),
    )
    .await?;
    let mut refreshed = credential(data(response_json(response).await?)?)?;
    for field in ["domain", "userId", "enterpriseId", "label"] {
        if refreshed[field].is_null() {
            refreshed[field] = value[field].clone();
        }
    }
    Ok(refreshed)
}
pub(super) fn models(value: &Value) -> Result<Vec<String>> {
    required(value, &["access"])?;
    Ok(MODELS.iter().map(|s| (*s).to_owned()).collect())
}
pub(super) async fn chat(
    value: &Value,
    model: &str,
    messages: Vec<Value>,
    on_delta: impl Fn(String) -> Result<()> + Send,
) -> Result<String> {
    let messages = super::validate_messages(&messages)?;
    let client = client()?;
    let response = send(
        credential_headers(request(&client, Method::POST, CHAT_URL), value)?
            .json(&json!({"model":model,"messages":messages,"stream":true})),
    )
    .await?;
    consume_chat(response, on_delta).await
}
async fn consume_chat(
    response: Response,
    on_delta: impl Fn(String) -> Result<()> + Send,
) -> Result<String> {
    super::require_sse(&response)?;
    let mut received = 0usize;
    let mut stream = response
        .bytes_stream()
        .map(move |part| {
            let bytes = part.map_err(|_| "WorkBuddy stream interrupted.".to_owned())?;
            received = received.saturating_add(bytes.len());
            if received > MAX_RESPONSE {
                return Err("WorkBuddy stream exceeds size limit.".to_owned());
            }
            Ok(bytes)
        })
        .eventsource();
    let mut output = String::new();
    while let Some(event) = stream.next().await {
        let event = event.map_err(|_| "WorkBuddy stream interrupted.".to_owned())?;
        if event.data == "[DONE]" {
            return Ok(output);
        }
        let value: Value = serde_json::from_str(&event.data)
            .map_err(|_| "WorkBuddy stream returned invalid JSON.".to_owned())?;
        if event.event == "error" || value.get("error").is_some() {
            return Err("WorkBuddy completion failed.".into());
        }
        let choices = value["choices"]
            .as_array()
            .ok_or_else(|| "WorkBuddy returned an invalid completion.".to_owned())?;
        for choice in choices {
            if choice["index"].as_u64().unwrap_or(0) != 0 {
                continue;
            }
            if choice["delta"].get("tool_calls").is_some()
                || choice["delta"].get("function_call").is_some()
            {
                return Err("WorkBuddy returned unsupported tool calls.".into());
            }
            if let Some(delta) = choice["delta"].get("content").filter(|v| !v.is_null()) {
                let delta = delta
                    .as_str()
                    .ok_or_else(|| "WorkBuddy returned an invalid text delta.".to_owned())?;
                output.push_str(delta);
                on_delta(delta.to_owned())?;
            }
        }
    }
    Err("WorkBuddy stream ended before its completion marker.".into())
}
#[cfg(test)]
#[path = "../../../test/oauth_workbuddy.rs"]
mod tests;

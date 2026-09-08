use std::{path::PathBuf, process::Stdio};
use serde_json::Value;
use tauri::AppHandle;
use tokio::{io::{AsyncBufReadExt, AsyncWriteExt, BufReader}, process::Command};
use tokio_util::sync::CancellationToken;

pub async fn run<F>(app: &AppHandle, request: Value, cancellation: CancellationToken, on_event: F) -> Result<Value, String>
where F: Fn(Value) -> Result<(), String> + Send {
    let path = sidecar(app)?;
    let mut child = Command::new(path).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true).spawn().map_err(|e| e.to_string())?;
    let mut stdin = child.stdin.take().ok_or("OAuth host stdin unavailable.")?;
    stdin.write_all(serde_json::to_string(&request).map_err(|e| e.to_string())?.as_bytes()).await.map_err(|e| e.to_string())?;
    stdin.write_all(b"\n").await.map_err(|e| e.to_string())?;
    drop(stdin);
    let stdout = child.stdout.take().ok_or("OAuth host stdout unavailable.")?;
    let mut lines = BufReader::new(stdout).lines();
    loop {
        tokio::select! {
            _ = cancellation.cancelled() => { child.kill().await.map_err(|e| e.to_string())?; return Err("OAuth operation cancelled.".into()); }
            line = lines.next_line() => match line.map_err(|e| e.to_string())? {
                Some(value) => { let event: Value = serde_json::from_str(&value).map_err(|e| format!("Invalid OAuth host response: {e}"))?; match event.get("type").and_then(Value::as_str) {
                    Some("url") => { if let Some(url) = event.get("text").and_then(Value::as_str) { open_url(url)?; on_event(event)?; } }
                    Some("delta") | Some("done") => { on_event(event)?; }
                    Some("result") => { child.wait().await.map_err(|e| e.to_string())?; return Ok(event.get("value").cloned().unwrap_or(Value::Null)); }
                    Some("error") => return Err(event.get("error").and_then(Value::as_str).unwrap_or("OAuth host failed.").into()),
                    _ => {}
                } }
                None => return Err("OAuth host exited without a result.".into()),
            }
        }
    }
}
fn sidecar(app: &AppHandle) -> Result<PathBuf, String> { let name = if cfg!(target_os = "windows") { "oauth-host-x86_64-pc-windows-msvc.exe" } else if cfg!(target_arch = "aarch64") { "oauth-host-aarch64-apple-darwin" } else { "oauth-host-x86_64-apple-darwin" }; Ok(app.path().resource_dir().map_err(|e| e.to_string())?.join(name)) }
fn open_url(url: &str) -> Result<(), String> {
    let parsed = url::Url::parse(url).map_err(|_| "OAuth host returned an invalid URL.".to_string())?;
    if parsed.scheme() != "https" || parsed.username() != "" || parsed.password().is_some() { return Err("OAuth host returned an unsafe URL.".into()); }
    open::that_detached(parsed.as_str()).map_err(|e| format!("Unable to open OAuth URL: {e}"))
}

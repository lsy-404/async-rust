use crate::AppState;
use serde_json::Value;

const MODELS_DEV_URL: &str = "https://models.dev/api.json";

pub fn usable_models(payload: &Value, provider_id: &str) -> Result<Vec<String>, String> {
    let provider = payload
        .get("providers")
        .unwrap_or(payload)
        .get(provider_id)
        .ok_or_else(|| "models.dev 中没有该供应商。".to_owned())?;
    let mut models = provider
        .get("models")
        .and_then(Value::as_object)
        .ok_or_else(|| "models.dev 模型目录格式无效。".to_owned())?
        .iter()
        .filter(|(_, model)| {
            model.get("deprecated") != Some(&Value::Bool(true))
                && model.get("status").and_then(Value::as_str) != Some("deprecated")
                && model.get("tool_call") == Some(&Value::Bool(true))
                && model.pointer("/modalities/output").and_then(Value::as_array)
                    .is_some_and(|output| output.iter().any(|item| item == "text"))
        })
        .filter_map(|(key, model)| model.get("id").and_then(Value::as_str).or(Some(key)))
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    models.sort();
    models.dedup();
    if models.is_empty() { return Err("models.dev 中没有可用的文本工具模型。".into()); }
    Ok(models)
}

pub async fn refresh(state: &AppState) -> Result<(), String> {
    let payload = state.client.get(MODELS_DEV_URL).send().await
        .map_err(|_| "无法刷新 models.dev 目录。")?
        .error_for_status().map_err(|_| "models.dev 目录请求失败。")?
        .json::<Value>().await.map_err(|_| "models.dev 返回无效目录。")?;
    let entries = payload.get("providers").unwrap_or(&payload).as_object()
        .ok_or_else(|| "models.dev 目录格式无效。".to_owned())?;
    let mut db = state.db()?;
    let tx = db.transaction().map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM providers WHERE id NOT IN ('workbuddy','traecode')", [])
        .map_err(|e| e.to_string())?;
    for (id, provider) in entries {
        let Some(api) = provider.get("api").and_then(Value::as_str).filter(|url| url.starts_with("https://")) else { continue };
        let Ok(models) = usable_models(&payload, id) else { continue };
        let name = provider.get("name").and_then(Value::as_str).filter(|name| !name.trim().is_empty()).unwrap_or(id);
        tx.execute("INSERT INTO providers(id,name,base_url,models_json) VALUES(?,?,?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,base_url=excluded.base_url,models_json=excluded.models_json",
            (id, name, api.trim_end_matches('/'), serde_json::to_string(&models).map_err(|_| "目录编码失败。")?))
            .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
#[path = "../../test/models_catalog.rs"]
mod tests;

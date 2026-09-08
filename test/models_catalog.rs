use super::*;
use serde_json::json;

#[test]
fn models_dev_binding_keeps_only_supported_text_tool_models() {
    let payload = json!({"openai":{"models":{
        "good":{"id":"good","tool_call":true,"modalities":{"output":["text"]}},
        "image":{"tool_call":true,"modalities":{"output":["image"]}},
        "old":{"tool_call":true,"deprecated":true,"modalities":{"output":["text"]}}
    }}});
    assert_eq!(usable_models(&payload, "openai").unwrap(), vec!["good"]);
}

#[test]
fn catalog_provider_data_is_not_treated_as_a_local_model_list() {
    let payload = json!({"fixture":{"models":{"tool":{"tool_call":true,"modalities":{"output":["text"]}}}}});
    assert_eq!(usable_models(&payload, "fixture").unwrap(), vec!["tool"]);
}

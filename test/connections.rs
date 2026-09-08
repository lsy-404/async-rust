use super::*;
use tempfile::TempDir;
fn setup() -> (TempDir, AppState) {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path().join("test.db")).unwrap();
    for id in ["first", "second", "workbuddy"] {
        state
            .db()
            .unwrap()
            .execute(
                "INSERT OR IGNORE INTO providers(id,name,base_url,models_json) VALUES(?,?,?,'[]')",
                params![id, id, "http://localhost"],
            )
            .unwrap();
    }
    (dir, state)
}
fn add(state: &AppState, label: &str, weight: u32) -> Credential {
    let mut item = Credential::new("first", "api-key", label.into(), vec!["model".into()]);
    item.weight = weight;
    save(state, &item, &format!("secret-{label}")).unwrap();
    item
}
#[test]
fn multiple_credentials_persist_and_remove_independently() {
    let (dir, state) = setup();
    let a = add(&state, "a", 1);
    let b = add(&state, "b", 1);
    let reopened = AppState::open(dir.path().join("test.db")).unwrap();
    assert_eq!(list(&reopened, "first").unwrap().len(), 2);
    assert_eq!(
        reopened.key(&a.id).unwrap().get_password().unwrap(),
        "secret-a"
    );
    remove(&reopened, "first", &a.id, "api-key").unwrap();
    assert!(reopened.key(&a.id).unwrap().get_password().is_err());
    assert_eq!(list(&reopened, "first").unwrap()[0].id, b.id);
    assert_eq!(
        reopened.key(&b.id).unwrap().get_password().unwrap(),
        "secret-b"
    );
    assert!(!serde_json::to_string(&b.view()).unwrap().contains("secret"));
}
#[test]
fn provider_ownership_and_method_are_enforced_before_secret_mutation() {
    let (_dir, state) = setup();
    let a = add(&state, "a", 1);
    assert!(get(&state, "second", &a.id).is_err());
    assert!(update(&state, "second", &a.id, false, 1).is_err());
    assert!(remove(&state, "first", &a.id, "oauth").is_err());
    assert!(remove(&state, "second", &a.id, "api-key").is_err());
    let mut wrong = a.clone();
    wrong.provider_id = "second".into();
    assert!(save(&state, &wrong, "replacement").is_err());
    assert_eq!(
        state.key(&a.id).unwrap().get_password().unwrap(),
        "secret-a"
    );
    assert!(update(&state, "first", &a.id, true, 0).is_err());
    assert!(update(&state, "first", &a.id, true, 101).is_err());
}
#[test]
fn round_robin_persists_position_and_failover_preserves_order() {
    let (dir, state) = setup();
    let a = add(&state, "a", 1);
    let b = add(&state, "b", 1);
    assert_eq!(candidates(&state, "first", "model").unwrap()[0].id, a.id);
    let state = AppState::open(dir.path().join("test.db")).unwrap();
    assert_eq!(candidates(&state, "first", "model").unwrap()[0].id, b.id);
    set_strategy(&state, "first", "failover").unwrap();
    for _ in 0..3 {
        assert_eq!(candidates(&state, "first", "model").unwrap()[0].id, a.id);
    }
}
#[test]
fn weighted_round_robin_distributes_exact_weight_and_persists() {
    let (dir, state) = setup();
    let a = add(&state, "a", 3);
    let b = add(&state, "b", 1);
    set_strategy(&state, "first", "weighted-round-robin").unwrap();
    let mut selected = Vec::new();
    for index in 0..8 {
        let reopened = AppState::open(dir.path().join("test.db")).unwrap();
        let items = candidates(
            if index % 2 == 0 { &state } else { &reopened },
            "first",
            "model",
        )
        .unwrap();
        assert_eq!(items.len(), 2);
        selected.push(items[0].id.clone());
    }
    assert_eq!(selected.iter().filter(|id| **id == a.id).count(), 6);
    assert_eq!(selected.iter().filter(|id| **id == b.id).count(), 2);
}
#[test]
fn eligibility_filters_disabled_unhealthy_cooling_models_and_oauth_toggle() {
    let (_dir, state) = setup();
    let a = add(&state, "a", 1);
    let b = add(&state, "b", 1);
    update(&state, "first", &a.id, false, 1).unwrap();
    assert_eq!(candidates(&state, "first", "model").unwrap()[0].id, b.id);
    report_error(&state, "first", &b.id, Failure::Temporary).unwrap();
    assert!(candidates(&state, "first", "model").unwrap().is_empty());
    report_success(&state, "first", &b.id).unwrap();
    assert!(candidates(&state, "first", "absent").unwrap().is_empty());
    report_error(&state, "first", &b.id, Failure::Authentication).unwrap();
    assert!(!has_credential(&state, "first").unwrap());
    let item = Credential::new("workbuddy", "oauth", "Account".into(), vec!["model".into()]);
    save(&state, &item, "{}").unwrap();
    assert!(has_credential(&state, "workbuddy").unwrap());
    set_oauth_enabled(&state, "workbuddy", false).unwrap();
    assert!(!has_credential(&state, "workbuddy").unwrap());
    assert!(candidates(&state, "workbuddy", "model").unwrap().is_empty());
}
#[test]
fn metadata_reads_do_not_require_secret_file_and_failed_refresh_preserves_models() {
    let (dir, state) = setup();
    let item = add(&state, "a", 1);
    std::fs::write(
        dir.path().join("credentials/credentials.json"),
        "invalid file",
    )
    .unwrap();
    assert!(has_credential(&state, "first").unwrap());
    report_error(&state, "first", &item.id, Failure::Authentication).unwrap();
    catalog_result(&state, "first", 1).unwrap();
    assert_eq!(
        get(&state, "first", &item.id).unwrap().models,
        vec!["model"]
    );
    assert_eq!(catalog_status(&state).unwrap()["state"], "error");
}
#[test]
fn failed_secret_write_rolls_back_new_metadata() {
    let (dir, state) = setup();
    let existing = add(&state, "a", 1);
    std::fs::write(dir.path().join("credentials/credentials.json"), "corrupt").unwrap();
    let item = Credential::new("first", "api-key", "New".into(), vec!["new-model".into()]);
    assert!(save(&state, &item, "new-secret").is_err());
    let items = list(&state, "first").unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, existing.id);
}

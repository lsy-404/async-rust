use super::*;
#[test]
fn independent_credentials_persist_and_delete_without_touching_others() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("credentials");
    let first = Entry::new(root.clone(), "first").unwrap();
    let second = Entry::new(root.clone(), "second").unwrap();
    assert!(matches!(first.get_password(), Err(Error::NoEntry)));
    first.set_password("fixture-one").unwrap();
    second.set_password("fixture-two").unwrap();
    assert_eq!(
        Entry::new(root.clone(), "first")
            .unwrap()
            .get_password()
            .unwrap(),
        "fixture-one"
    );
    first.delete_credential().unwrap();
    assert_eq!(second.get_password().unwrap(), "fixture-two");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(root.join("credentials.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}
#[test]
fn concurrent_updates_preserve_all_entries_and_corruption_is_not_overwritten() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("credentials");
    std::thread::scope(|scope| {
        for number in 0..12 {
            let root = root.clone();
            scope.spawn(move || {
                Entry::new(root, &number.to_string())
                    .unwrap()
                    .set_password("fixture")
                    .unwrap()
            });
        }
    });
    let values: BTreeMap<String, String> =
        serde_json::from_slice(&fs::read(root.join("credentials.json")).unwrap()).unwrap();
    assert_eq!(values.len(), 12);
    fs::write(root.join("credentials.json"), b"broken").unwrap();
    assert!(Entry::new(root.clone(), "new")
        .unwrap()
        .set_password("fixture")
        .is_err());
    assert_eq!(fs::read(root.join("credentials.json")).unwrap(), b"broken");
}

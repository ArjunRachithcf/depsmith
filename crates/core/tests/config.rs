//! Saving and extending a target selection in `depsmith.toml`.
use depsmith_core::config::{add_targets, save_targets, settings};
use depsmith_core::Error;
use std::fs;
use tempfile::tempdir;

fn saved(root: &std::path::Path) -> Vec<String> {
    settings(root, &serde_json::json!({})).unwrap().targets
}

#[test]
fn saving_a_selection_creates_the_configuration() {
    let root = tempdir().unwrap();
    save_targets(root.path(), &["pixi:pixi.toml".into()]).unwrap();
    assert_eq!(saved(root.path()), ["pixi:pixi.toml"]);
}

#[test]
fn saving_keeps_existing_options_and_comments() {
    let root = tempdir().unwrap();
    let path = root.path().join("depsmith.toml");
    let original = "# repository policy\n[options]\ntimeout_seconds = 30 # slow CI\n";
    fs::write(&path, original).unwrap();
    save_targets(root.path(), &["a".into(), "b".into()]).unwrap();
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains(original.trim_end()), "{text}");
    let config = settings(root.path(), &serde_json::json!({})).unwrap();
    assert_eq!(config.targets, ["a", "b"]);
    assert_eq!(config.options.timeout_seconds, 30);
}

#[test]
fn an_empty_selection_is_replaced_but_an_existing_one_is_kept() {
    let root = tempdir().unwrap();
    let path = root.path().join("depsmith.toml");
    fs::write(&path, "targets = [] # none yet\n").unwrap();
    save_targets(root.path(), &["a".into()]).unwrap();
    assert_eq!(saved(root.path()), ["a"]);
    let before = fs::read_to_string(&path).unwrap();
    let error = save_targets(root.path(), &["b".into()]).unwrap_err();
    assert!(
        matches!(&error, Error::Invalid(m) if m.contains("already selects")),
        "{error}"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), before);
}

#[test]
fn adding_targets_appends_only_new_ones_and_keeps_the_rest() {
    let root = tempdir().unwrap();
    let path = root.path().join("depsmith.toml");
    fs::write(
        &path,
        "# repository policy\ntargets = [\"a\"] # reviewed\n[options]\ntimeout_seconds = 30\n",
    )
    .unwrap();
    let added = add_targets(root.path(), &["a".into(), "b".into()]).unwrap();
    assert_eq!(added, ["b"]);
    assert_eq!(saved(root.path()), ["a", "b"]);
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("# repository policy"), "{text}");
    assert!(text.contains("# reviewed"), "{text}");
    assert_eq!(
        settings(root.path(), &serde_json::json!({}))
            .unwrap()
            .options
            .timeout_seconds,
        30
    );
}

#[test]
fn adding_to_a_missing_configuration_creates_it_and_adding_nothing_writes_nothing() {
    let root = tempdir().unwrap();
    assert!(add_targets(root.path(), &[]).unwrap().is_empty());
    assert!(!root.path().join("depsmith.toml").exists());
    assert_eq!(add_targets(root.path(), &["a".into()]).unwrap(), ["a"]);
    assert_eq!(saved(root.path()), ["a"]);
}

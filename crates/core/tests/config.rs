//! Saving and extending a target selection in `depsmith.toml`.
use depsmith_core::config::{add_targets, settings};
use std::fs;
use tempfile::tempdir;

fn saved(root: &std::path::Path) -> Vec<String> {
    settings(root, &serde_json::json!({})).unwrap().targets
}

#[test]
fn adding_to_a_configuration_without_targets_keeps_its_options_and_comments() {
    let root = tempdir().unwrap();
    let path = root.path().join("depsmith.toml");
    let original = "# repository policy\n[options]\ntimeout_seconds = 30 # slow CI\n";
    fs::write(&path, original).unwrap();
    add_targets(root.path(), &["a".into(), "b".into()]).unwrap();
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains(original.trim_end()), "{text}");
    let config = settings(root.path(), &serde_json::json!({})).unwrap();
    assert_eq!(config.targets, ["a", "b"]);
    assert_eq!(config.options.timeout_seconds, 30);
}

#[test]
fn targets_that_are_not_a_list_are_rejected_unchanged() {
    let root = tempdir().unwrap();
    let path = root.path().join("depsmith.toml");
    fs::write(&path, "targets = \"a\"\n").unwrap();
    assert!(add_targets(root.path(), &["b".into()]).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "targets = \"a\"\n");
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

//! What `depsmith init` reports about the selected targets besides their
//! tools: the selected adapters' capabilities, and depsmith declared as a
//! project dependency.
use depsmith_core::{Engine, UpdateOptions};
use std::fs;

fn repository() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let workflows = root.path().join(".github/workflows");
    fs::create_dir_all(&workflows).unwrap();
    fs::write(
        workflows.join("ci.yml"),
        "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n",
    )
    .unwrap();
    fs::write(
        root.path().join("pyproject.toml"),
        "[project]\nname = \"demo\"\nversion = \"1.0\"\ndependencies = [\"depsmith>=0.1.0rc1\", \"six\"]\n",
    )
    .unwrap();
    fs::write(root.path().join("uv.lock"), "version = 1\n").unwrap();
    root
}

fn init(root: &std::path::Path, selected: &[String]) -> serde_json::Value {
    Engine::default()
        .init(root, selected, &UpdateOptions::default(), &mut |_, _| {
            Ok(false)
        })
        .unwrap()
}

fn managers(report: &serde_json::Value) -> Vec<&str> {
    report["adapters"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["manager"].as_str().unwrap())
        .collect()
}

#[test]
fn init_reports_the_capabilities_of_the_selected_adapters_only() {
    let root = repository();
    let report = init(root.path(), &["uv:pyproject.toml".into()]);
    assert_eq!(managers(&report), ["uv"]);
    assert!(
        report["adapters"][0]["cooldown"]["status"].is_string(),
        "{report:#}"
    );
    let all = init(root.path(), &[]);
    assert_eq!(managers(&all), ["github-actions", "uv"]);
}

#[test]
fn init_warns_when_a_target_declares_depsmith_itself() {
    let root = repository();
    let report = init(root.path(), &[]);
    let warnings: Vec<&str> = report["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w.as_str().unwrap())
        .collect();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].starts_with("uv:pyproject.toml declares depsmith"),
        "{}",
        warnings[0]
    );
    assert!(
        warnings[0].contains("uv tool install depsmith"),
        "{}",
        warnings[0]
    );

    fs::write(
        root.path().join("pyproject.toml"),
        "[project]\nname = \"demo\"\nversion = \"1.0\"\ndependencies = [\"six\"]\n",
    )
    .unwrap();
    assert_eq!(init(root.path(), &[])["warnings"], serde_json::json!([]));
}

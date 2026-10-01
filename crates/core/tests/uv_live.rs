//! Opt-in acceptance test with the real uv against PyPI: a pinned package is
//! suggested with index evidence, accepted, applied and rechecked.
use depsmith_core::{apply, Engine, UpdateOptions};
use std::{collections::BTreeMap, fs};

fn options(accept: &[&str]) -> UpdateOptions {
    UpdateOptions {
        tools: BTreeMap::from([("uv".into(), std::env::var("UV").unwrap())]),
        accept: accept.iter().map(|a| a.to_string()).collect(),
        ..Default::default()
    }
}

#[test]
#[ignore = "queries PyPI"]
fn live_project_suggests_accepts_applies_and_rechecks() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("pyproject.toml"),
        "[project]\nname = \"live\"\nversion = \"0.1.0\"\nrequires-python = \">=3.10\"\n\
         dependencies = [\n    \"six==1.15.0\", # pinned\n    \"idna\",\n]\n\n[tool.uv]\n",
    )
    .unwrap();
    let engine = Engine::default();
    let target = ["uv:pyproject.toml".into()];

    let preview = engine.prepare(root.path(), &target, options(&[])).unwrap();
    assert!(preview.failures.is_empty(), "{:?}", preview.failures);
    let six = preview
        .suggestions
        .iter()
        .find(|s| s.package == "six")
        .unwrap_or_else(|| panic!("no six suggestion: {:?}", preview.suggestions));
    assert_eq!(six.requirement, "==1.15.0");
    assert!(
        six.evidence[0].contains("https://files.pythonhosted.org/"),
        "{:?}",
        six.evidence
    );
    assert!(preview
        .changes
        .iter()
        .any(|c| c.path.as_os_str() == "uv.lock"));

    let accepted = engine
        .prepare(root.path(), &target, options(&["six"]))
        .unwrap();
    assert!(accepted.failures.is_empty(), "{:?}", accepted.failures);
    let manifest = accepted
        .changes
        .iter()
        .find(|c| c.path.as_os_str() == "pyproject.toml")
        .unwrap();
    assert!(
        manifest.after.contains("\"six==1.") && manifest.after.contains("\", # pinned\n"),
        "{}",
        manifest.after
    );
    assert!(
        !manifest.after.contains("six==1.15.0"),
        "{}",
        manifest.after
    );
    apply(&accepted, false).unwrap();

    let recheck = engine.prepare(root.path(), &target, options(&[])).unwrap();
    assert!(recheck.failures.is_empty(), "{:?}", recheck.failures);
    assert!(recheck.changes.is_empty(), "{:?}", recheck.changes);
    assert!(
        !recheck.suggestions.iter().any(|s| s.package == "six"),
        "{:?}",
        recheck.suggestions
    );
}

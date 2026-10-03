//! Opt-in acceptance test with the real npm against registry.npmjs.org: a
//! pinned package is suggested with registry evidence, accepted, applied and
//! rechecked.
use depsmith_core::{apply, Engine, UpdateOptions};
use std::{collections::BTreeMap, fs};

fn options(accept: &[&str]) -> UpdateOptions {
    UpdateOptions {
        tools: BTreeMap::from([("npm".into(), std::env::var("NPM").unwrap())]),
        accept: accept.iter().map(|a| a.to_string()).collect(),
        ..Default::default()
    }
}

#[test]
#[ignore = "queries registry.npmjs.org"]
fn live_package_suggests_accepts_applies_and_rechecks() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("package.json"),
        "{\n  \"name\": \"live\",\n  \"private\": true,\n  \"dependencies\": {\n    \"ms\": \"2.0.0\",\n    \"debug\": \"^3.0.0\"\n  }\n}\n",
    )
    .unwrap();
    fs::write(root.path().join(".npmrc"), "fund=false\n").unwrap();
    let engine = Engine::default();
    let target = ["npm:package.json".into()];
    // Without a lock the package is not a target yet: create one first.
    std::process::Command::new(std::env::var("NPM").unwrap())
        .args([
            "install",
            "--package-lock-only",
            "--ignore-scripts",
            "--no-audit",
        ])
        .current_dir(root.path())
        .status()
        .unwrap();

    let preview = engine.prepare(root.path(), &target, options(&[])).unwrap();
    assert!(preview.failures.is_empty(), "{:?}", preview.failures);
    let ms = preview
        .suggestions
        .iter()
        .find(|s| s.package == "ms")
        .unwrap_or_else(|| panic!("no ms suggestion: {:?}", preview.suggestions));
    assert_eq!(ms.requirement, "2.0.0");
    assert!(
        ms.evidence[0].contains("https://registry.npmjs.org/ms/-/ms-"),
        "{:?}",
        ms.evidence
    );

    let accepted = engine
        .prepare(root.path(), &target, options(&["ms"]))
        .unwrap();
    assert!(accepted.failures.is_empty(), "{:?}", accepted.failures);
    let manifest = accepted
        .changes
        .iter()
        .find(|c| c.path.as_os_str() == "package.json")
        .unwrap();
    assert!(
        !manifest.after.contains("\"ms\": \"2.0.0\""),
        "{}",
        manifest.after
    );
    assert!(
        manifest.after.contains("\"ms\": \"2."),
        "{}",
        manifest.after
    );
    apply(&accepted, false).unwrap();

    let recheck = engine.prepare(root.path(), &target, options(&[])).unwrap();
    assert!(recheck.failures.is_empty(), "{:?}", recheck.failures);
    assert!(recheck.changes.is_empty(), "{:?}", recheck.changes);
    assert!(
        !recheck.suggestions.iter().any(|s| s.package == "ms"),
        "{:?}",
        recheck.suggestions
    );
}

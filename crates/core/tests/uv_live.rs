//! Opt-in acceptance tests with the real uv against PyPI: a pinned package is
//! suggested with index evidence, accepted, applied and rechecked, and a
//! nested package moves to the exact release a precise update names.
use depsmith_core::{apply, Engine, UpdateOptions};
use std::{collections::BTreeMap, fs};

fn options(accept: &[&str]) -> UpdateOptions {
    UpdateOptions {
        tools: BTreeMap::from([("uv".into(), std::env::var("UV").unwrap())]),
        accept: accept.iter().map(|a| a.to_string()).collect(),
        ..Default::default()
    }
}

/// The version of `name` that `lock` (a `uv.lock`) records.
fn locked<'a>(lock: &'a str, name: &str) -> &'a str {
    let entry = format!("name = \"{name}\"\nversion = \"");
    let start = lock
        .find(&entry)
        .unwrap_or_else(|| panic!("{name} unlocked"))
        + entry.len();
    &lock[start..start + lock[start..].find('"').unwrap()]
}

#[test]
#[ignore = "queries PyPI"]
fn live_precise_update_moves_a_nested_package_to_the_exact_release() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("pyproject.toml"),
        "[project]\nname = \"live\"\nversion = \"0.1.0\"\nrequires-python = \">=3.10\"\n\
         dependencies = [\"requests\"]\n\n[tool.uv]\n",
    )
    .unwrap();
    // urllib3 is a dependency of requests, never declared here: lock an old one.
    let uv = std::env::var("UV").unwrap();
    for args in [
        &["lock"][..],
        &["lock", "--upgrade-package", "urllib3==2.0.7"],
    ] {
        let status = std::process::Command::new(&uv)
            .args(args)
            .current_dir(root.path())
            .status()
            .unwrap();
        assert!(status.success(), "uv {args:?}");
    }
    let baseline = fs::read_to_string(root.path().join("uv.lock")).unwrap();
    assert_eq!(locked(&baseline, "urllib3"), "2.0.7");

    let options = UpdateOptions {
        precise: vec![depsmith_core::PreciseUpdate {
            package: "urllib3".into(),
            version: "2.2.1".into(),
        }],
        ..options(&[])
    };
    let proposal = Engine::default()
        .prepare(root.path(), &["uv:pyproject.toml".into()], options)
        .unwrap();
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    let lock = proposal
        .changes
        .iter()
        .find(|c| c.path.as_os_str() == "uv.lock")
        .unwrap_or_else(|| panic!("uv.lock unchanged: {:?}", proposal.changes));
    assert_eq!(locked(&lock.after, "urllib3"), "2.2.1");
    // Only the requested package moved.
    assert_eq!(
        locked(&lock.after, "requests"),
        locked(&baseline, "requests")
    );
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

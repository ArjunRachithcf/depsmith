//! Opt-in acceptance test with the real cargo against crates.io: a pinned
//! crate is suggested with index evidence, accepted, applied and rechecked,
//! and a crate held back by `rust-version` is reported.
use depsmith_core::{apply, Engine, UpdateOptions};
use std::{collections::BTreeMap, fs};

fn options(accept: &[&str]) -> UpdateOptions {
    UpdateOptions {
        tools: BTreeMap::from([("cargo".into(), std::env::var("CARGO").unwrap())]),
        accept: accept.iter().map(|a| a.to_string()).collect(),
        ..Default::default()
    }
}

#[test]
#[ignore = "queries crates.io"]
fn live_crate_suggests_accepts_applies_and_rechecks() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("src")).unwrap();
    fs::write(root.path().join("src/lib.rs"), "").unwrap();
    fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname = \"live\"\nversion = \"0.1.0\"\nedition = \"2021\"\nrust-version = \"1.60\"\n\n\
         [dependencies]\nitoa = \"=1.0.1\" # pinned\nonce_cell = \"1\"\n",
    )
    .unwrap();
    let engine = Engine::default();
    let target = ["cargo:Cargo.toml".into()];

    let preview = engine.prepare(root.path(), &target, options(&[])).unwrap();
    assert!(preview.failures.is_empty(), "{:?}", preview.failures);
    let itoa = preview
        .suggestions
        .iter()
        .find(|s| s.package == "itoa")
        .unwrap_or_else(|| panic!("no itoa suggestion: {:?}", preview.suggestions));
    assert_eq!(itoa.requirement, "=1.0.1");
    assert!(
        itoa.evidence[0].starts_with("crates.io: 1.")
            && itoa.evidence[0].contains("https://crates.io/api/v1/crates/itoa/"),
        "{:?}",
        itoa.evidence
    );
    let held = preview
        .suggestions
        .iter()
        .find(|s| s.package == "once_cell")
        .unwrap_or_else(|| panic!("once_cell not held back: {:?}", preview.suggestions));
    assert!(held.reason.contains("requires Rust"), "{}", held.reason);

    let accepted = engine
        .prepare(root.path(), &target, options(&["itoa"]))
        .unwrap();
    assert!(accepted.failures.is_empty(), "{:?}", accepted.failures);
    let manifest = accepted
        .changes
        .iter()
        .find(|c| c.path.as_os_str() == "Cargo.toml")
        .unwrap();
    assert!(
        manifest.after.contains("itoa = \"=1.") && manifest.after.contains("\" # pinned\n"),
        "{}",
        manifest.after
    );
    assert!(!manifest.after.contains("\"=1.0.1\""), "{}", manifest.after);
    assert!(fs::read_to_string(root.path().join("Cargo.toml"))
        .unwrap()
        .contains("\"=1.0.1\""));
    apply(&accepted, false).unwrap();

    let recheck = engine.prepare(root.path(), &target, options(&[])).unwrap();
    assert!(recheck.failures.is_empty(), "{:?}", recheck.failures);
    assert!(recheck.changes.is_empty(), "{:?}", recheck.changes);
    assert!(
        !recheck.suggestions.iter().any(|s| s.package == "itoa"),
        "{:?}",
        recheck.suggestions
    );
}

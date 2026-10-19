//! Opt-in acceptance tests with the real cargo against crates.io: a pinned
//! crate is suggested with index evidence, accepted, applied and rechecked,
//! a crate held back by `rust-version` is reported, and a nested crate moves
//! to the exact release a precise update names. A security update moves a
//! transitive crate precisely or by name.
use depsmith_core::{apply, Engine, UpdateOptions};
#[cfg(unix)]
use depsmith_core::{FixOutcome, SecurityTarget};
use std::{collections::BTreeMap, fs};

mod support;

fn options(accept: &[&str]) -> UpdateOptions {
    UpdateOptions {
        tools: BTreeMap::from([("cargo".into(), std::env::var("CARGO").unwrap())]),
        accept: accept.iter().map(|a| a.to_string()).collect(),
        ..Default::default()
    }
}

/// The version of `name` that `lock` (a `Cargo.lock`) records.
fn locked<'a>(lock: &'a str, name: &str) -> &'a str {
    let entry = format!("name = \"{name}\"\nversion = \"");
    let start = lock
        .find(&entry)
        .unwrap_or_else(|| panic!("{name} unlocked"))
        + entry.len();
    &lock[start..start + lock[start..].find('"').unwrap()]
}

#[test]
#[ignore = "queries crates.io"]
fn live_precise_update_moves_a_nested_crate_to_the_exact_release() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("src")).unwrap();
    fs::write(root.path().join("src/lib.rs"), "").unwrap();
    fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname = \"live\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [dependencies]\nserde_json = \"1\"\n",
    )
    .unwrap();
    // itoa is serde_json's dependency, never declared here: lock an old one.
    let cargo = std::env::var("CARGO").unwrap();
    for args in [
        &["generate-lockfile"][..],
        &["update", "-p", "itoa", "--precise", "1.0.5"],
    ] {
        let status = std::process::Command::new(&cargo)
            .args(args)
            .current_dir(root.path())
            .status()
            .unwrap();
        assert!(status.success(), "cargo {args:?}");
    }
    let baseline = fs::read_to_string(root.path().join("Cargo.lock")).unwrap();
    assert_eq!(locked(&baseline, "itoa"), "1.0.5");

    let options = UpdateOptions {
        precise: vec![depsmith_core::PreciseUpdate {
            package: "itoa".into(),
            version: "1.0.9".into(),
        }],
        ..options(&[])
    };
    let proposal = Engine::default()
        .prepare(root.path(), &["cargo:Cargo.toml".into()], options)
        .unwrap();
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    let lock = proposal
        .changes
        .iter()
        .find(|c| c.path.as_os_str() == "Cargo.lock")
        .unwrap_or_else(|| panic!("Cargo.lock unchanged: {:?}", proposal.changes));
    assert_eq!(locked(&lock.after, "itoa"), "1.0.9");
    // Only the requested crate moved.
    assert_eq!(
        locked(&lock.after, "serde_json"),
        locked(&baseline, "serde_json")
    );
}

#[test]
#[cfg(unix)]
#[ignore = "queries crates.io"]
fn live_security_update_moves_a_transitive_crate_precisely_or_by_name() {
    let root = tempfile::tempdir().unwrap();
    let tools = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("src")).unwrap();
    fs::write(root.path().join("src/lib.rs"), "").unwrap();
    fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname = \"live\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [dependencies]\nserde_json = \"1\"\n",
    )
    .unwrap();
    let tool = std::env::var("CARGO").unwrap();
    for args in [
        &["generate-lockfile"][..],
        &["update", "-p", "itoa", "--precise", "1.0.5"],
    ] {
        let status = std::process::Command::new(&tool)
            .args(args)
            .current_dir(root.path())
            .status()
            .unwrap();
        assert!(status.success(), "cargo {args:?}");
    }
    let grype = support::grype(
        tools.path(),
        "GHSA-live",
        "github:language:rust",
        "pkg:cargo/itoa@1.0.5",
        "1.0.9",
    );
    for target in [SecurityTarget::Lowest, SecurityTarget::Newest] {
        let mut options = options(&[]);
        options.tools.insert("grype".into(), grype.clone());
        let options = UpdateOptions {
            security_only: true,
            security_target: target,
            ..options
        };
        let proposal = Engine::default()
            .prepare(root.path(), &["cargo:Cargo.toml".into()], options)
            .unwrap();
        assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
        let fix = &proposal.security[0];
        assert_eq!(fix.outcome, FixOutcome::Fixed, "{target:?}: {}", fix.reason);
        assert_eq!(
            fix.paths,
            [vec!["serde_json".to_owned(), "itoa".to_owned()]]
        );
        let lock = proposal
            .changes
            .iter()
            .find(|c| c.path.as_os_str() == "Cargo.lock")
            .unwrap_or_else(|| panic!("Cargo.lock unchanged: {:?}", proposal.changes));
        let moved = locked(&lock.after, "itoa");
        if target == SecurityTarget::Lowest {
            assert_eq!(moved, "1.0.9");
        } else {
            assert_ne!(
                moved, "1.0.5",
                "newest moves by name past the locked release"
            );
        }
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

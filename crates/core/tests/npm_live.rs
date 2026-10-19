//! Opt-in acceptance test with the real npm against registry.npmjs.org: a
//! pinned package is suggested with registry evidence, accepted, applied and
//! rechecked, a transitive package named by a precise update moves, and a
//! security update moves a transitive package by name.
use depsmith_core::{apply, Engine, PreciseUpdate, UpdateOptions};
#[cfg(unix)]
use depsmith_core::{FixOutcome, SecurityTarget};
use std::{collections::BTreeMap, fs, path::Path};

mod support;

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
    // Without a lock the package is not a target yet: create one first. On
    // Windows, NPM is npm.cmd: the extensionless npm is a POSIX shell shim.
    let created = std::process::Command::new(std::env::var("NPM").unwrap())
        .args([
            "install",
            "--package-lock-only",
            "--ignore-scripts",
            "--no-audit",
        ])
        .current_dir(root.path())
        .status()
        .unwrap();
    assert!(created.success(), "npm install: {created}");

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

/// A project whose only dependency, chalk 4.1.2, requires `ansi-styles
/// ^4.1.0`.
const CHALK: &str = "{\n  \"name\": \"live\",\n  \"private\": true,\n  \"dependencies\": {\n    \"chalk\": \"^4.1.2\"\n  }\n}\n";

/// Write [`CHALK`] to `root`, locked with ansi-styles at 4.2.1 although
/// 4.3.0 is within chalk's range: lock it with an override, then drop the
/// override, and the lock stays valid.
fn lock_older_ansi_styles(root: &Path) {
    fs::write(
        root.join("package.json"),
        CHALK.replace(
            "\n  }\n}",
            "\n  },\n  \"overrides\": {\n    \"ansi-styles\": \"4.2.1\"\n  }\n}",
        ),
    )
    .unwrap();
    fs::write(root.join(".npmrc"), "fund=false\n").unwrap();
    let created = std::process::Command::new(std::env::var("NPM").unwrap())
        .args([
            "install",
            "--package-lock-only",
            "--ignore-scripts",
            "--no-audit",
        ])
        .current_dir(root)
        .status()
        .unwrap();
    assert!(created.success(), "npm install: {created}");
    fs::write(root.join("package.json"), CHALK).unwrap();
    assert_eq!(locked_ansi_styles(root), "4.2.1");
}

/// The version of the hoisted ansi-styles in `root`'s `package-lock.json`.
fn locked_ansi_styles(root: &Path) -> String {
    let text = fs::read_to_string(root.join("package-lock.json")).unwrap();
    let lock: serde_json::Value = serde_json::from_str(&text).unwrap();
    lock["packages"]["node_modules/ansi-styles"]["version"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// npm has no precise update, so a precise request falls back to `npm update
/// NAME`. ADR 0004 asks each adapter to show that its tool moves a nested
/// package named this way: chalk 4.1.2 requires `ansi-styles ^4.1.0`, locked
/// here at 4.2.1 although 4.3.0 is within that range.
#[test]
#[ignore = "queries registry.npmjs.org"]
fn precise_update_of_a_nested_package_moves_it_by_name() {
    let root = tempfile::tempdir().unwrap();
    let manifest = CHALK;
    lock_older_ansi_styles(root.path());
    let locked = || locked_ansi_styles(root.path());

    let options = UpdateOptions {
        precise: vec![PreciseUpdate {
            package: "ansi-styles".into(),
            version: "4.3.0".into(),
        }],
        ..options(&[])
    };
    let proposal = Engine::default()
        .prepare(root.path(), &["npm:package.json".into()], options)
        .unwrap();
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    assert!(
        proposal
            .validation
            .iter()
            .any(|v| v.contains("npm has no precise update; ansi-styles moves")),
        "{:?}",
        proposal.validation
    );
    assert!(
        proposal.dependencies.iter().any(|d| d
            .after
            .as_ref()
            .is_some_and(|p| p.name == "ansi-styles" && p.version == "4.3.0")),
        "{:?}",
        proposal.dependencies
    );
    // The declaration is not edited to make up for the missing precise update.
    assert!(
        !proposal
            .changes
            .iter()
            .any(|c| c.path.as_os_str() == "package.json"),
        "{:?}",
        proposal.changes
    );
    apply(&proposal, false).unwrap();
    assert_eq!(locked(), "4.3.0");
    assert_eq!(
        fs::read_to_string(root.path().join("package.json")).unwrap(),
        manifest
    );
}

/// npm moves a transitive package a security update names, whatever the
/// security target, by `npm update NAME` (it has no precise update).
#[test]
#[cfg(unix)]
#[ignore = "queries registry.npmjs.org"]
fn live_security_update_moves_a_transitive_package_by_name() {
    let root = tempfile::tempdir().unwrap();
    let tools = tempfile::tempdir().unwrap();
    lock_older_ansi_styles(root.path());
    let grype = support::grype(
        tools.path(),
        "GHSA-live",
        "github:language:javascript",
        "pkg:npm/ansi-styles@4.2.1",
        "4.3.0",
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
            .prepare(root.path(), &["npm:package.json".into()], options)
            .unwrap();
        assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
        let fix = &proposal.security[0];
        assert_eq!(fix.outcome, FixOutcome::Fixed, "{target:?}: {}", fix.reason);
        assert_eq!(
            fix.paths,
            [vec!["chalk".to_owned(), "ansi-styles".to_owned()]]
        );
        assert!(
            proposal.dependencies.iter().any(|d| d
                .after
                .as_ref()
                .is_some_and(|p| p.name == "ansi-styles" && p.version != "4.2.1")),
            "{:?}",
            proposal.dependencies
        );
    }
}

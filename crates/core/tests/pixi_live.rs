//! Opt-in acceptance tests requiring Pixi and access to conda-forge/PyPI.
use depsmith_core::{apply, Engine, UpdateOptions};
use std::fs;

#[test]
#[ignore = "downloads Python/build dependencies and runs native Pixi"]
fn editable_source_resolves_in_stage_without_installing_into_original() {
    editable_roundtrip(false);
}

#[test]
#[ignore = "downloads Python/build dependencies and runs native Pixi and Git"]
fn scm_editable_uses_staged_history_without_a_pretend_version() {
    editable_roundtrip(true);
}

fn editable_roundtrip(scm: bool) {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("local")).unwrap();
    fs::write(
        root.path().join("pixi.toml"),
        r#"
[workspace]
name = "source-build-fixture"
channels = ["conda-forge"]
platforms = ["linux-64", "win-64"]
[dependencies]
python = "3.12.*"
setuptools = "*"
setuptools_scm = "*"
wheel = "*"
[pypi-dependencies]
updater-local-fixture = { path = "local", editable = true }
[pypi-options]
no-build-isolation = ["updater-local-fixture"]
"#,
    )
    .unwrap();
    fs::write(
        root.path().join("local/pyproject.toml"),
        r#"
[project]
name = "updater-local-fixture"
dynamic = ["version"]
[build-system]
requires = ["setuptools", "wheel", "setuptools_scm"]
build-backend = "setuptools.build_meta"
"#,
    )
    .unwrap();
    if scm {
        use std::io::Write;
        fs::OpenOptions::new()
            .append(true)
            .open(root.path().join("local/pyproject.toml"))
            .unwrap()
            .write_all(b"\n[tool.setuptools_scm]\nroot = '..'\n")
            .unwrap();
    } else {
        fs::write(
            root.path().join("local/setup.py"),
            "from setuptools import setup\nsetup(version='1.0.0')\n",
        )
        .unwrap();
    }
    fs::write(root.path().join("local/fixture.py"), "VALUE = 1\n").unwrap();
    if scm {
        for args in [
            vec!["init"],
            vec!["add", "."],
            vec!["commit", "-m", "fixture"],
            vec!["tag", "v1.2.3"],
        ] {
            let output = std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                ])
                .args(args)
                .current_dir(root.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    let options = UpdateOptions {
        pixi: std::env::var("PIXI").unwrap_or("pixi".into()),
        timeout_seconds: 240,
        ..Default::default()
    };
    let engine = Engine::default();
    let targets = ["pixi:pixi.toml".into()];
    let proposal = engine
        .prepare(root.path(), &targets, options.clone())
        .unwrap();
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    assert!(!root.path().join("pixi.lock").exists());
    assert!(!root.path().join(".pixi").exists());
    apply(&proposal, false).unwrap();
    let lock = fs::read_to_string(root.path().join("pixi.lock")).unwrap();
    let inventory = depsmith_core::inventory::pixi_inventory(&lock).unwrap();
    for platform in ["linux-64", "win-64"] {
        assert!(
            inventory.iter().any(|p| p.name == "updater-local-fixture"
                && p.version == "unknown"
                && p.platform == platform),
            "{inventory:?}"
        );
    }
    let recheck = engine.prepare(root.path(), &targets, options).unwrap();
    assert!(recheck.failures.is_empty(), "{:?}", recheck.failures);
    assert!(recheck.changes.is_empty());
    assert!(!root.path().join(".pixi").exists());
}

#[test]
#[ignore = "runs native Pixi and queries pypi.org"]
fn pypi_pin_cites_newer_release_from_the_index() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("pixi.toml"),
        r#"
[workspace]
name = "pypi-evidence-fixture"
channels = ["conda-forge"]
platforms = ["linux-64"]
[dependencies]
python = ">=3.12"
[pypi-dependencies]
six = "==1.15.0"
idna = "<100"
"#,
    )
    .unwrap();
    let options = UpdateOptions {
        pixi: std::env::var("PIXI").unwrap_or_else(|_| "pixi".into()),
        ..Default::default()
    };
    let proposal = Engine::default()
        .prepare(root.path(), &["pixi:pixi.toml".into()], options)
        .unwrap();
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    let names: Vec<_> = proposal
        .suggestions
        .iter()
        .map(|s| s.package.as_str())
        .collect();
    assert_eq!(names, ["six"], "{:?}", proposal.suggestions);
    let six = &proposal.suggestions[0];
    assert!(
        six.reason.contains("excludes a newer release"),
        "{}",
        six.reason
    );
    assert!(
        six.evidence
            .iter()
            .any(|e| e.starts_with("https://pypi.org/simple: ")
                && e.contains("(newest allowed 1.15.0)")
                && e.contains("sha256:")),
        "{:?}",
        six.evidence
    );
}

#[test]
#[ignore = "runs native Pixi and queries pypi.org"]
fn accepted_suggestions_rewrite_resolve_apply_and_recheck() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("pixi.toml"),
        r#"[workspace]
name = "accept-fixture"
channels = ["conda-forge"]
platforms = ["linux-64"]
[dependencies]
python = "3.12.*"
[pypi-dependencies]
six = "==1.15.0"  # pinned for reasons
"#,
    )
    .unwrap();
    let pixi = std::env::var("PIXI").unwrap_or_else(|_| "pixi".into());
    let options = |accept: &[&str]| UpdateOptions {
        pixi: pixi.clone(),
        accept: accept.iter().map(|a| a.to_string()).collect(),
        ..Default::default()
    };
    let engine = Engine::default();
    let proposal = engine
        .prepare(
            root.path(),
            &["pixi:pixi.toml".into()],
            options(&["six", "python=3.13.*"]),
        )
        .unwrap();
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    let manifest = proposal
        .changes
        .iter()
        .find(|c| c.path.ends_with("pixi.toml"))
        .expect("manifest change");
    assert!(
        manifest.after.contains("python = \"3.13.*\"\n"),
        "{}",
        manifest.after
    );
    let six_line = manifest
        .after
        .lines()
        .find(|l| l.starts_with("six"))
        .unwrap();
    assert!(
        !six_line.contains("1.15.0") && six_line.ends_with("  # pinned for reasons"),
        "{six_line}"
    );
    assert!(
        proposal
            .validation
            .iter()
            .any(|v| v.contains("accepted six ==1.15.0 -> ==")
                && v.contains("https://pypi.org/simple: ")),
        "{:?}",
        proposal.validation
    );
    assert!(
        proposal
            .validation
            .iter()
            .any(|v| v.contains("accepted python 3.12.* -> 3.13.* (explicit replacement)")),
        "{:?}",
        proposal.validation
    );
    assert!(proposal.dependencies.iter().any(|d| d
        .after
        .as_ref()
        .is_some_and(|p| p.name == "six" && p.version != "1.15.0")));
    assert!(
        !proposal.suggestions.iter().any(|s| s.package == "six"),
        "{:?}",
        proposal.suggestions
    );
    // Preview never touches the source.
    assert!(fs::read_to_string(root.path().join("pixi.toml"))
        .unwrap()
        .contains("==1.15.0"));
    apply(&proposal, false).unwrap();
    let recheck = engine
        .prepare(root.path(), &["pixi:pixi.toml".into()], options(&[]))
        .unwrap();
    assert!(recheck.failures.is_empty(), "{:?}", recheck.failures);
    assert!(
        recheck.changes.is_empty(),
        "{:?}",
        recheck.changes.iter().map(|c| &c.diff).collect::<Vec<_>>()
    );
}

/// Regression from a real integration run: `pixi search` and index pages ignore
/// the manifest's `exclude-newer`, so evidence must apply it.
#[test]
#[ignore = "runs native Pixi and queries pypi.org"]
fn release_age_cutoff_is_respected_by_evidence_and_acceptance() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("pixi.toml"),
        r#"[workspace]
name = "cutoff-fixture"
channels = ["conda-forge"]
platforms = ["linux-64"]
exclude-newer = "2021-01-01"
[dependencies]
python = "3.9.*"
[pypi-dependencies]
six = "==1.15.0"
"#,
    )
    .unwrap();
    let pixi = std::env::var("PIXI").unwrap_or_else(|_| "pixi".into());
    let options = |accept: &[&str]| UpdateOptions {
        pixi: pixi.clone(),
        accept: accept.iter().map(|a| a.to_string()).collect(),
        ..Default::default()
    };
    let engine = Engine::default();
    let proposal = engine
        .prepare(root.path(), &["pixi:pixi.toml".into()], options(&[]))
        .unwrap();
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    assert!(
        !proposal.suggestions.iter().any(|s| s.package == "six"),
        "{:?}",
        proposal.suggestions
    );
    let accepted = engine
        .prepare(root.path(), &["pixi:pixi.toml".into()], options(&["six"]))
        .unwrap();
    assert_eq!(accepted.failures.len(), 1, "{:?}", accepted.failures);
    assert_eq!(accepted.failures[0].code, 2);
    assert!(
        accepted.failures[0].message.contains("nothing to accept"),
        "{}",
        accepted.failures[0].message
    );
}

#[test]
#[ignore = "runs native Pixi and queries pypi.org"]
fn project_requirement_accept_round_trip() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("pyproject.toml"),
        r#"[project]
name = "project-accept-fixture"
version = "0.1.0"
requires-python = ">=3.12"
dependencies = [
  "six==1.15.0 ; python_version >= '3.8'", # pinned for reasons
]
[tool.pixi.workspace]
channels = ["conda-forge"]
platforms = ["linux-64"]
[tool.pixi.dependencies]
python = "3.12.*"
"#,
    )
    .unwrap();
    let pixi = std::env::var("PIXI").unwrap_or_else(|_| "pixi".into());
    let options = |accept: &[&str]| UpdateOptions {
        pixi: pixi.clone(),
        accept: accept.iter().map(|a| a.to_string()).collect(),
        ..Default::default()
    };
    let targets = ["pixi:pyproject.toml".to_string()];
    let engine = Engine::default();
    let proposal = engine.prepare(root.path(), &targets, options(&[])).unwrap();
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    let six = proposal
        .suggestions
        .iter()
        .find(|s| s.package == "six")
        .expect("[project] pin suggested");
    assert_eq!(six.requirement, "==1.15.0");
    assert!(
        six.evidence
            .iter()
            .any(|e| e.starts_with("https://pypi.org/simple: ")),
        "{:?}",
        six.evidence
    );
    let accepted = engine
        .prepare(root.path(), &targets, options(&["six"]))
        .unwrap();
    assert!(accepted.failures.is_empty(), "{:?}", accepted.failures);
    let manifest = accepted
        .changes
        .iter()
        .find(|c| c.path.ends_with("pyproject.toml"))
        .expect("manifest change");
    let line = manifest
        .after
        .lines()
        .find(|l| l.contains("\"six=="))
        .unwrap();
    assert!(
        !line.contains("1.15.0")
            && line.ends_with(" ; python_version >= '3.8'\", # pinned for reasons"),
        "{line}"
    );
    apply(&accepted, false).unwrap();
    let recheck = engine.prepare(root.path(), &targets, options(&[])).unwrap();
    assert!(recheck.failures.is_empty(), "{:?}", recheck.failures);
    assert!(recheck.changes.is_empty(), "{:?}", recheck.changes);
    assert!(!recheck.suggestions.iter().any(|s| s.package == "six"));
}

#[test]
#[ignore = "runs native Pixi and downloads conda-forge/PyPI metadata"]
fn mixed_conda_and_pypi_lock_covers_every_platform() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("pixi.toml"),
        r#"[workspace]
name = "mixed-platform-fixture"
channels = ["conda-forge"]
platforms = ["linux-64", "win-64", "osx-arm64"]
[dependencies]
python = "3.12.*"
zlib = "*"
[pypi-dependencies]
six = "*"
"#,
    )
    .unwrap();
    let options = UpdateOptions {
        pixi: std::env::var("PIXI").unwrap_or_else(|_| "pixi".into()),
        ..Default::default()
    };
    let proposal = Engine::default()
        .prepare(root.path(), &["pixi:pixi.toml".into()], options)
        .unwrap();
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    let resolved: std::collections::BTreeSet<(String, String, String)> = proposal
        .dependencies
        .iter()
        .filter_map(|d| d.after.as_ref())
        .map(|p| (p.platform.clone(), p.ecosystem.clone(), p.name.clone()))
        .collect();
    for platform in ["linux-64", "win-64", "osx-arm64"] {
        for (ecosystem, name) in [("conda", "python"), ("conda", "zlib"), ("pypi", "six")] {
            assert!(
                resolved.contains(&(platform.into(), ecosystem.into(), name.into())),
                "{platform} {ecosystem}:{name} missing from {resolved:?}"
            );
        }
    }
}

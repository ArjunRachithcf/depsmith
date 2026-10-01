//! Capability declarations and their enforcement before any work starts.
use depsmith_core::{
    adapter::{tool_status, Adapter, Candidate, Support},
    Engine, Error, Result, Target, UpdateOptions,
};
use std::{fs, path::Path};
use tempfile::tempdir;

/// Declares no capabilities; preparing it means the engine failed to enforce them.
struct Undeclared;
impl Adapter for Undeclared {
    fn spec(&self) -> depsmith_core::adapter::AdapterSpec {
        depsmith_core::adapter::AdapterSpec::new("fixture", &["project.toml"])
    }
    fn detects(&self, path: &Path, _: &str) -> bool {
        path == Path::new("project.toml")
    }
    fn prepare(&self, _: &Path, _: &Target, _: &UpdateOptions) -> Result<Candidate> {
        panic!("engine must reject unsupported options before preparing")
    }
}

fn fixture(options: UpdateOptions) -> Error {
    let root = tempdir().unwrap();
    fs::write(root.path().join("project.toml"), "").unwrap();
    Engine::new(vec![Box::new(Undeclared)])
        .prepare(root.path(), &["fixture:project.toml".into()], options)
        .unwrap_err()
}

#[test]
fn unsupported_options_fail_before_the_adapter_runs() {
    let cases = [
        (
            UpdateOptions {
                cooldown_days: Some(7),
                ..Default::default()
            },
            "cooldown",
        ),
        (
            UpdateOptions {
                install: true,
                ..Default::default()
            },
            "install",
        ),
        (
            UpdateOptions {
                refresh_git: true,
                ..Default::default()
            },
            "refresh-git",
        ),
        (
            UpdateOptions {
                upgrade: true,
                packages: vec!["x".into()],
                ..Default::default()
            },
            "upgrade",
        ),
        (
            UpdateOptions {
                packages: vec!["x".into()],
                ..Default::default()
            },
            "package selection",
        ),
    ];
    for (options, needle) in cases {
        let error = fixture(options);
        assert!(
            matches!(&error, Error::Invalid(m) if m.contains(needle) && m.contains("fixture")),
            "{needle}: {error}"
        );
    }
}

#[test]
fn inapplicable_options_are_reported_not_ignored() {
    let root = tempdir().unwrap();
    let workflows = root.path().join(".github/workflows");
    fs::create_dir_all(&workflows).unwrap();
    // Local action only: no remote lookups are needed.
    fs::write(
        workflows.join("ci.yml"),
        "on: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: ./local\n",
    )
    .unwrap();
    let options = UpdateOptions {
        install: true,
        refresh_git: true,
        ..Default::default()
    };
    let proposal = Engine::default()
        .prepare(
            root.path(),
            &["github-actions:.github/workflows/ci.yml".into()],
            options,
        )
        .unwrap();
    for flag in ["--install", "--refresh-git"] {
        let note = format!("github-actions:.github/workflows/ci.yml: {flag} not applicable");
        assert!(
            proposal.validation.iter().any(|v| v.starts_with(&note)),
            "{note} missing from {:?}",
            proposal.validation
        );
    }
}

#[test]
fn builtin_adapters_declare_capabilities_and_tested_tools() {
    let specs = Engine::default().specs();
    let pixi = specs.iter().find(|s| s.manager == "pixi").unwrap();
    let caps = &pixi.capabilities;
    assert_eq!(caps.package_selection, Support::Supported);
    assert_eq!(caps.lockfile, Support::Supported);
    assert!(matches!(&caps.cooldown, Support::Unsupported(hint) if hint.contains("exclude-newer")));
    let tool = &pixi.tools[0];
    assert_eq!(tool.name, "pixi");
    assert!(tool.tested_versions.contains(&"0.80.0".to_string()));

    let actions = specs
        .iter()
        .find(|s| s.manager == "github-actions")
        .unwrap();
    let caps = &actions.capabilities;
    assert_eq!(caps.install_validation, Support::NotApplicable);
    assert_eq!(caps.git_refresh, Support::NotApplicable);
    assert_eq!(caps.constraint_changes, Support::Supported);
    assert!(actions.tools.is_empty());
}

#[test]
fn tool_versions_are_tested_or_untested_never_guessed_compatible() {
    assert_eq!(
        tool_status("pixi 0.80.0\n", &["0.80.0"]),
        ("tested", Some("0.80.0".into()))
    );
    assert_eq!(
        tool_status("pixi 0.81.0", &["0.80.0"]),
        ("untested", Some("0.81.0".into()))
    );
    assert_eq!(
        tool_status("no version here", &["0.80.0"]),
        ("untested", None)
    );
    assert_eq!(
        tool_status("cargo 1.98.1 (797e8a9bc 2026-08-05)", &["1.98.1"]),
        ("tested", Some("1.98.1".into()))
    );
    assert_eq!(
        tool_status("2.9.0", &["2.9.0"]),
        ("tested", Some("2.9.0".into()))
    );
}

#[test]
fn doctor_reports_capabilities_and_unavailable_tools() {
    let options = UpdateOptions {
        pixi: "depsmith-nonexistent-executable".into(),
        grype: "depsmith-nonexistent-executable".into(),
        tools: ["cargo", "conda-lock", "conda", "uv"]
            .map(|t| (t.into(), "depsmith-nonexistent-executable".into()))
            .into(),
        ..Default::default()
    };
    let report = depsmith_core::doctor(&options);
    assert_eq!(report["schema_version"], 1);
    let adapters = report["adapters"].as_array().unwrap();
    assert!(adapters
        .iter()
        .any(|a| a["manager"] == "pixi" && a["cooldown"]["status"] == "unsupported"));
    for tool in report["tools"].as_array().unwrap() {
        assert_eq!(tool["status"], "unavailable", "{tool}");
        assert!(tool["tested_versions"]
            .as_array()
            .is_some_and(|v| !v.is_empty()));
    }
}

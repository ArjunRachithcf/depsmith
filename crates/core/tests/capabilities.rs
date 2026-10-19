//! Capability declarations and their enforcement before any work starts.
use depsmith_core::{
    adapter::{tool_status, Adapter, AdapterSpec, Candidate, Capabilities, Support},
    conformance, Engine, Error, PreciseUpdate, Result, Target, UpdateOptions,
};
use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    sync::{Arc, Mutex},
};
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
        (
            // Without package selection there is nothing to fall back on.
            UpdateOptions {
                precise: vec![precise("x", "1.0.0")],
                ..Default::default()
            },
            "precise update",
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
    assert!(
        matches!(&caps.precise_update, Support::Unsupported(hint) if hint.contains("newest release")),
        "{:?}",
        caps.precise_update
    );
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
    assert_eq!(caps.precise_update, Support::NotApplicable);
    assert!(actions.tools.is_empty());
}

#[test]
fn precise_update_is_declared_where_the_native_tool_has_it() {
    let specs = Engine::default().specs();
    let precise = |manager: &str| {
        specs
            .iter()
            .find(|s| s.manager == manager)
            .unwrap_or_else(|| panic!("no {manager} adapter"))
            .capabilities
            .precise_update
            .clone()
    };
    for manager in ["cargo", "uv"] {
        assert_eq!(precise(manager), Support::Supported, "{manager}");
    }
    for manager in ["npm", "pixi", "conda"] {
        assert!(
            matches!(&precise(manager), Support::Unsupported(hint)
                if hint.contains("newest release its constraints allow")),
            "{manager}: {:?}",
            precise(manager)
        );
    }
}

fn precise(package: &str, version: &str) -> PreciseUpdate {
    PreciseUpdate {
        package: package.into(),
        version: version.into(),
    }
}

/// Declares `precise` support and package selection, declares no direct
/// dependencies, and records the options it prepares with.
struct Recording {
    precise: Support,
    seen: Arc<Mutex<Vec<UpdateOptions>>>,
}
impl Adapter for Recording {
    fn spec(&self) -> AdapterSpec {
        AdapterSpec {
            capabilities: Capabilities {
                package_selection: Support::Supported,
                lockfile: Support::Supported,
                precise_update: self.precise.clone(),
                ..Capabilities::undeclared()
            },
            ..AdapterSpec::new("fixture", &["project.toml"])
        }
    }
    fn detects(&self, path: &Path, _: &str) -> bool {
        path == Path::new("project.toml")
    }
    fn select(&self, _: &Path, _: &Target, _: &[String]) -> Result<BTreeMap<String, String>> {
        Ok(BTreeMap::new())
    }
    fn prepare(&self, _: &Path, _: &Target, options: &UpdateOptions) -> Result<Candidate> {
        self.seen.lock().unwrap().push(options.clone());
        Ok(Candidate::default())
    }
}

/// Prepare a precise update of the transitive `nested` to 2.0.1 with an
/// adapter declaring `support`; returns the proposal's notes and the
/// options the adapter prepared with, if it ran.
fn precise_run(support: Support) -> (Vec<String>, Vec<UpdateOptions>) {
    let root = tempdir().unwrap();
    fs::write(root.path().join("project.toml"), "").unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let adapter = Recording {
        precise: support,
        seen: seen.clone(),
    };
    let options = UpdateOptions {
        precise: vec![precise("nested", "2.0.1")],
        ..Default::default()
    };
    let proposal = Engine::new(vec![Box::new(adapter)])
        .prepare(root.path(), &["fixture:project.toml".into()], options)
        .unwrap();
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    let seen = seen.lock().unwrap().clone();
    (proposal.validation, seen)
}

#[test]
fn precise_updates_reach_the_adapter_unchanged_even_for_transitive_packages() {
    let (_, seen) = precise_run(Support::Supported);
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].precise.len(), 1);
    assert_eq!(seen[0].precise[0].package, "nested");
    assert_eq!(seen[0].precise[0].version, "2.0.1");
    assert!(seen[0].packages.is_empty(), "{:?}", seen[0].packages);
}

#[test]
fn without_precise_update_the_package_moves_as_far_as_its_constraints_allow() {
    let (notes, seen) = precise_run(Support::Unsupported("no precise update".into()));
    assert_eq!(seen.len(), 1);
    assert!(seen[0].precise.is_empty(), "{:?}", seen[0].precise);
    assert_eq!(seen[0].packages, ["nested"]);
    let note = "fixture:project.toml: fixture has no precise update; nested moves to the \
                newest release its constraints allow, not 2.0.1";
    assert!(notes.iter().any(|n| n == note), "{notes:?}");
}

#[test]
fn precise_updates_are_skipped_where_not_applicable() {
    let (notes, seen) = precise_run(Support::NotApplicable);
    assert!(seen.is_empty(), "the adapter ran with {seen:?}");
    assert!(
        notes
            .iter()
            .any(|n| n.starts_with("fixture:project.toml: precise update not applicable")),
        "{notes:?}"
    );
}

#[test]
fn precise_requests_need_a_package_and_a_version() {
    for (package, version) in [("", "1.0.0"), ("x", " ")] {
        let options = UpdateOptions {
            precise: vec![precise(package, version)],
            ..Default::default()
        };
        assert!(
            matches!(options.validate(), Err(Error::Invalid(m)) if m.contains("precise")),
            "{package:?} {version:?}"
        );
    }
}

#[test]
fn conformance_requires_precise_update_to_select_packages_and_lock() {
    /// Claims a precise update with nothing else to support it.
    struct Claims;
    impl Adapter for Claims {
        fn spec(&self) -> AdapterSpec {
            AdapterSpec {
                capabilities: Capabilities {
                    precise_update: Support::Supported,
                    ..Capabilities::undeclared()
                },
                ..AdapterSpec::new("fixture", &["project.toml"])
            }
        }
        fn detects(&self, path: &Path, _: &str) -> bool {
            path == Path::new("project.toml")
        }
        fn prepare(&self, _: &Path, _: &Target, _: &UpdateOptions) -> Result<Candidate> {
            Ok(Candidate::default())
        }
    }
    let fixture = conformance::Fixture {
        files: vec![("project.toml".into(), String::new())],
        target: "fixture:project.toml".into(),
        declared: vec![],
    };
    let problems = conformance::check(&|| Box::new(Claims) as Box<dyn Adapter>, &fixture)
        .expect_err("a precise update without package selection or a lockfile conforms");
    assert!(
        problems.iter().any(|p| p.contains("precise update")),
        "{problems:?}"
    );
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
        tools: ["cargo", "conda-lock", "conda", "uv", "npm"]
            .map(|t| (t.into(), "depsmith-nonexistent-executable".into()))
            .into(),
        ..Default::default()
    };
    let report = depsmith_core::doctor(std::path::Path::new("."), &options);
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

#[test]
fn transitive_updates_are_declared_only_where_live_tests_show_them() {
    let specs = Engine::default().specs();
    let transitive = |manager: &str| {
        specs
            .iter()
            .find(|s| s.manager == manager)
            .unwrap_or_else(|| panic!("no {manager} adapter"))
            .capabilities
            .transitive_updates
            .clone()
    };
    // Each has a live test moving a transitive package (`*_live.rs`).
    for manager in ["cargo", "uv", "npm", "pixi"] {
        assert_eq!(transitive(manager), Support::Supported, "{manager}");
    }
    assert!(
        matches!(&transitive("conda"), Support::Unsupported(hint) if !hint.is_empty()),
        "{:?}",
        transitive("conda")
    );
    assert_eq!(transitive("github-actions"), Support::NotApplicable);
}

#[test]
fn conformance_requires_transitive_updates_to_select_packages_and_lock() {
    /// Claims transitive updates with nothing else to support them.
    struct Claims;
    impl Adapter for Claims {
        fn spec(&self) -> AdapterSpec {
            AdapterSpec {
                capabilities: Capabilities {
                    transitive_updates: Support::Supported,
                    ..Capabilities::undeclared()
                },
                ..AdapterSpec::new("fixture", &["project.toml"])
            }
        }
        fn detects(&self, path: &Path, _: &str) -> bool {
            path == Path::new("project.toml")
        }
        fn prepare(&self, _: &Path, _: &Target, _: &UpdateOptions) -> Result<Candidate> {
            Ok(Candidate::default())
        }
    }
    let fixture = conformance::Fixture {
        files: vec![("project.toml".into(), String::new())],
        target: "fixture:project.toml".into(),
        declared: vec![],
    };
    let problems = conformance::check(&|| Box::new(Claims) as Box<dyn Adapter>, &fixture)
        .expect_err("transitive updates without package selection or a lockfile conform");
    assert!(
        problems.iter().any(|p| p.contains("transitive updates")),
        "{problems:?}"
    );
}

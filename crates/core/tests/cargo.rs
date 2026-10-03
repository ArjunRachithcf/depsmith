//! The Cargo adapter offline: lock-owner discovery, member declarations,
//! resolution with the real cargo on a path-only workspace, and suggestions,
//! `--accept` and MSRV hold-backs with a fixture registry and a stand-in cargo.
use depsmith_core::{adapter::Adapter, cargo::Cargo, conformance, Engine, Target, UpdateOptions};
use std::{collections::BTreeMap, fs, path::Path};

fn write(root: &Path, files: &[(&str, &str)]) {
    for (path, content) in files {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
}

fn package(name: &str, extra: &str) -> String {
    format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n{extra}")
}

const MEMBER: &str = r#"
[dependencies]
b = { path = "../b" }
serde = "1.0.100" # keep
json = { package = "serde_json", version = "1" }
local-git = { git = "https://example.invalid/g.git" }
shared = { workspace = true }
private = { version = "1", registry = "internal" }

[target.'cfg(unix)'.dependencies]
libc = "0.2"

[dev-dependencies]
tempfile = ">=3"
"#;

fn workspace(root: &Path) {
    write(
        root,
        &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"crates/*\"]\nexclude = [\"crates/skip\"]\n\n[workspace.dependencies]\nshared = \"=2.1.0\"\n",
            ),
            ("crates/a/Cargo.toml", &package("a", MEMBER)),
            ("crates/a/src/lib.rs", ""),
            ("crates/b/Cargo.toml", &package("b", "")),
            ("crates/b/src/lib.rs", ""),
            ("crates/skip/Cargo.toml", &package("skip", "")),
            ("tools/t/Cargo.toml", &package("t", "")),
        ],
    );
}

fn target(manifest: &str) -> Target {
    Target {
        id: format!("cargo:{manifest}"),
        manager: "cargo".into(),
        manifest: manifest.into(),
    }
}

#[test]
fn lock_owners_are_targets_and_members_are_declarations() {
    let root = tempfile::tempdir().unwrap();
    workspace(root.path());
    let ids: Vec<String> = Engine::default()
        .discover(root.path())
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .filter(|id| id.starts_with("cargo:"))
        .collect();
    assert_eq!(
        ids,
        [
            "cargo:Cargo.toml",
            "cargo:crates/skip/Cargo.toml",
            "cargo:tools/t/Cargo.toml"
        ]
    );
    let declared: Vec<(String, String, String, String)> = Cargo::default()
        .declarations(root.path(), &target("Cargo.toml"))
        .unwrap()
        .into_iter()
        .map(|d| {
            assert_eq!(d.ecosystem, "cargo");
            (
                d.file.to_string_lossy().replace('\\', "/"),
                d.location,
                d.package,
                d.requirement,
            )
        })
        .collect();
    let expected = [
        (
            "Cargo.toml",
            "workspace.dependencies.shared",
            "shared",
            "=2.1.0",
        ),
        ("crates/a/Cargo.toml", "dependencies.b", "b", ""),
        (
            "crates/a/Cargo.toml",
            "dependencies.json",
            "serde_json",
            "1",
        ),
        (
            "crates/a/Cargo.toml",
            "dependencies.local-git",
            "local-git",
            "",
        ),
        (
            "crates/a/Cargo.toml",
            "dependencies.serde",
            "serde",
            "1.0.100",
        ),
        ("crates/a/Cargo.toml", "dependencies.shared", "shared", ""),
        (
            "crates/a/Cargo.toml",
            "dev-dependencies.tempfile",
            "tempfile",
            ">=3",
        ),
        (
            "crates/a/Cargo.toml",
            "target.cfg(unix).dependencies.libc",
            "libc",
            "0.2",
        ),
    ]
    .map(|(f, l, p, r)| (f.into(), l.into(), p.into(), r.into()));
    assert_eq!(declared, expected);
    let selected = Cargo::default()
        .select(
            root.path(),
            &target("Cargo.toml"),
            &[
                "Serde_Json".into(),
                "json".into(),
                "private".into(),
                "nope".into(),
            ],
        )
        .unwrap();
    assert_eq!(
        selected,
        BTreeMap::from([
            ("Serde_Json".into(), "serde_json".into()),
            ("json".into(), "serde_json".into()),
            ("private".into(), "private".into()),
        ])
    );
}

fn real_cargo() -> UpdateOptions {
    UpdateOptions {
        tools: BTreeMap::from([("cargo".into(), std::env::var("CARGO").unwrap())]),
        ..Default::default()
    }
}

#[test]
fn path_only_workspace_resolves_with_the_real_cargo() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"a\", \"b\"]\nresolver = \"2\"\n",
            ),
            (
                "a/Cargo.toml",
                &package("a", "[dependencies]\nb = { path = \"../b\" }\n"),
            ),
            ("a/src/lib.rs", ""),
            ("b/Cargo.toml", &package("b", "")),
            ("b/src/lib.rs", ""),
        ],
    );
    let proposal = Engine::default()
        .prepare(root.path(), &["cargo:Cargo.toml".into()], real_cargo())
        .unwrap();
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    let paths: Vec<_> = proposal.changes.iter().map(|c| c.path.clone()).collect();
    assert_eq!(paths, [Path::new("Cargo.lock")]);
    assert!(proposal.changes[0].after.contains("name = \"a\""));
    assert!(proposal
        .validation
        .iter()
        .any(|v| v == "cargo:Cargo.toml: resolved and lock-consistent"));
    // Workspace members are the project itself, not dependencies to scan.
    assert!(
        proposal.dependencies.is_empty(),
        "{:?}",
        proposal.dependencies
    );
}

#[test]
fn cargo_passes_the_conformance_suite() {
    let fixture = conformance::Fixture {
        files: vec![
            (
                "Cargo.toml".into(),
                "[workspace]\nmembers = [\"a\"]\n".into(),
            ),
            (
                "a/Cargo.toml".into(),
                package("a", "[dependencies]\nSerde = \"1.0\" # keep\n"),
            ),
            ("a/src/lib.rs".into(), String::new()),
        ],
        target: "cargo:Cargo.toml".into(),
        declared: vec![("serde".into(), "Serde".into())],
    };
    conformance::check(&|| Box::new(Cargo::default()) as Box<dyn Adapter>, &fixture).unwrap();
}

#[cfg(unix)]
mod stand_in {
    use super::*;
    use depsmith_core::{
        constraints::{FixtureRelease, RegistryConfig},
        Proposal,
    };
    use std::os::unix::fs::PermissionsExt;

    const LOCK: &str = "version = 4\n\n[[package]]\nname = \"a\"\nversion = \"0.1.0\"\n\n[[package]]\nname = \"once_cell\"\nversion = \"1.17.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"abc\"\n";

    /// A cargo that reports `version`, writes a fixed lock on `update` and
    /// reports once_cell as held back by the MSRV, in colour unless asked
    /// for `--color never`.
    fn cargo(dir: &Path, version: &str) -> String {
        let script = dir.join("cargo");
        let lock = dir.join("lock");
        fs::write(&lock, LOCK).unwrap();
        fs::write(
            &script,
            format!(
                "#!/bin/sh\ncase \"$1\" in\n  --version) echo 'cargo {version} (fake 2026-01-01)';;\n  update) cp '{}' Cargo.lock; line='Unchanged once_cell v1.17.0 (available: v1.21.3, requires Rust 1.70)'; case \"$*\" in *'--color never'*) echo \"    $line\" >&2;; *) printf '\\033[1m    %s\\033[0m\\n' \"$line\" >&2;; esac;;\n  metadata) echo '{{}}';;\n  *) exit 2;;\nesac\n",
                lock.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        script.to_string_lossy().into_owned()
    }

    fn registry() -> RegistryConfig {
        let release = |version: &str| FixtureRelease {
            version: version.into(),
            url: format!("https://crates.io/api/v1/crates/serde/{version}/download"),
            sha256: format!("sum-{version}"),
        };
        RegistryConfig::Fixture {
            releases: BTreeMap::from([(
                "serde".into(),
                vec![release("1.0.100"), release("1.0.200"), release("2.0.0")],
            )]),
            failing: vec![],
        }
    }

    fn prepare(requirement: &str, accept: &[&str], version: &str) -> Proposal {
        let root = tempfile::tempdir().unwrap();
        let tools = tempfile::tempdir().unwrap();
        write(
            root.path(),
            &[
                ("Cargo.toml", "[workspace]\nmembers = [\"a\"]\n"),
                (
                    "a/Cargo.toml",
                    &package(
                        "a",
                        &format!("[dependencies]\nserde = \"{requirement}\" # keep\n"),
                    ),
                ),
            ],
        );
        let engine = Engine::new(vec![Box::new(Cargo {
            registry: Some(registry()),
        })]);
        let options = UpdateOptions {
            tools: BTreeMap::from([("cargo".into(), cargo(tools.path(), version))]),
            accept: accept.iter().map(|a| a.to_string()).collect(),
            ..Default::default()
        };
        engine
            .prepare(root.path(), &["cargo:Cargo.toml".into()], options)
            .unwrap()
    }

    /// The RUSTUP_TOOLCHAIN a stand-in cargo saw while updating a crate,
    /// with or without a rust-toolchain.toml in the project.
    fn toolchain_seen(toolchain_file: bool) -> String {
        let root = tempfile::tempdir().unwrap();
        let tools = tempfile::tempdir().unwrap();
        write(root.path(), &[("Cargo.toml", &package("a", ""))]);
        if toolchain_file {
            write(
                root.path(),
                &[("rust-toolchain.toml", "[toolchain]\nchannel = \"1.80\"\n")],
            );
        }
        let seen = tools.path().join("seen");
        let script = tools.path().join("cargo");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\ncase \"$1\" in\n  --version) echo 'cargo 1.98.1 (fake 2026-01-01)';;\n  update) echo \"${{RUSTUP_TOOLCHAIN:-unset}}\" > '{}'; cp '{}' Cargo.lock;;\nesac\n",
                seen.display(),
                cargo(tools.path(), "1.98.1").replace("/cargo", "/lock")
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let mut options = UpdateOptions {
            tools: BTreeMap::from([("cargo".into(), script.to_string_lossy().into_owned())]),
            ..Default::default()
        };
        options.tool_envs.insert(
            "cargo".into(),
            vec![("RUSTUP_TOOLCHAIN".into(), "1.98.1".into())],
        );
        let proposal = Engine::new(vec![Box::new(Cargo::default())])
            .prepare(root.path(), &["cargo:Cargo.toml".into()], options)
            .unwrap();
        assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
        fs::read_to_string(seen).unwrap().trim().to_owned()
    }

    #[test]
    fn a_projects_toolchain_file_wins_over_the_installed_toolchain() {
        assert_eq!(toolchain_seen(false), "1.98.1");
        // Not depsmith's pin (the test harness may set its own).
        assert_ne!(toolchain_seen(true), "1.98.1");
    }

    #[test]
    fn capped_requirements_and_msrv_hold_backs_are_suggestions() {
        let proposal = prepare("1.0.100", &[], "1.98.1");
        assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
        let serde = proposal
            .suggestions
            .iter()
            .find(|s| s.package == "serde")
            .unwrap();
        assert_eq!(serde.requirement, "1.0.100");
        assert!(
            serde.evidence[0].contains("2.0.0 is excluded (newest allowed 1.0.200)"),
            "{:?}",
            serde.evidence
        );
        let held = proposal
            .suggestions
            .iter()
            .find(|s| s.package == "once_cell")
            .unwrap();
        assert!(
            held.reason.contains("requires Rust 1.70"),
            "{}",
            held.reason
        );
        let packages: Vec<_> = proposal
            .dependencies
            .iter()
            .filter_map(|d| d.after.as_ref())
            .map(|p| (p.name.as_str(), p.artifact.as_str()))
            .collect();
        assert_eq!(
            packages,
            [(
                "once_cell",
                "https://crates.io/api/v1/crates/once_cell/1.17.0/download"
            )]
        );
    }

    #[test]
    fn accepting_restyles_at_the_declared_precision() {
        let proposal = prepare("1.0", &["serde"], "1.98.1");
        assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
        let manifest = proposal
            .changes
            .iter()
            .find(|c| c.path == Path::new("a/Cargo.toml"))
            .unwrap();
        assert!(
            manifest.after.contains("serde = \"2.0\" # keep\n"),
            "{}",
            manifest.after
        );
        assert!(proposal
            .validation
            .iter()
            .any(|v| v.contains("accepted serde 1.0 -> 2.0 (fixture: 2.0.0 is excluded")));
        for (accept, needle) in [
            ("serde=not a requirement", "not a Cargo version requirement"),
            ("serde", "ambiguous"),
        ] {
            let requirement = if accept == "serde" { ">=1, <2" } else { "1.0" };
            let proposal = prepare(requirement, &[accept], "1.98.1");
            assert_eq!(
                proposal.failures.len(),
                1,
                "{accept}: {:?}",
                proposal.failures
            );
            assert_eq!(proposal.failures[0].code, 2);
            assert!(
                proposal.failures[0].message.contains(needle),
                "{}",
                proposal.failures[0].message
            );
        }
    }

    #[test]
    fn cargo_older_than_1_84_is_unsupported() {
        let proposal = prepare("1.0", &[], "1.80.0");
        assert_eq!(proposal.failures.len(), 1, "{:?}", proposal.failures);
        assert_eq!(proposal.failures[0].code, 2);
        assert!(
            proposal.failures[0].message.contains("1.84"),
            "{}",
            proposal.failures[0].message
        );
    }
}

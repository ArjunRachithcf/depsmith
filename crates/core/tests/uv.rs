//! The uv adapter offline: lock-owner discovery, workspace declarations,
//! rewrites, and suggestions, `--accept` and Git-pin preservation with a
//! fixture registry and a stand-in uv.
use depsmith_core::{adapter::Adapter, conformance, uv::Uv, Engine, Target};
use std::{fs, path::Path};

fn write(root: &Path, files: &[(&str, &str)]) {
    for (path, content) in files {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
}

fn project(name: &str, extra: &str) -> String {
    format!("[project]\nname = \"{name}\"\nversion = \"0.1.0\"\n{extra}")
}

const ROOT: &str = r#"[project]
name = "root"
version = "0.1.0"
dependencies = [
    "six==1.15.0", # pinned
    "member",
    "local",
    "iniconfig @ git+https://github.com/pytest-dev/iniconfig@v2.0.0",
    "private>=1",
]

[dependency-groups]
dev = ["packaging<24"]

[tool.uv]
dev-dependencies = ["pytest>=8"]

[tool.uv.sources]
member = { workspace = true }
local = { path = "vendor/local" }
private = { index = "internal" }

[tool.uv.workspace]
members = ["packages/*"]
exclude = ["packages/skip"]
"#;

fn workspace(root: &Path) {
    write(
        root,
        &[
            ("pyproject.toml", ROOT),
            (
                "packages/member/pyproject.toml",
                &project("member", "dependencies = [\"idna>=3\"]\n"),
            ),
            ("packages/skip/pyproject.toml", &project("skip", "")),
            ("packages/skip/uv.lock", "version = 1\n"),
            ("plain/pyproject.toml", &project("plain", "")),
            (
                "pixi/pyproject.toml",
                &project(
                    "pixi",
                    "[tool.pixi.workspace]\nchannels = []\nplatforms = []\n",
                ),
            ),
            ("tooled/pyproject.toml", &project("tooled", "[tool.uv]\n")),
        ],
    );
}

fn target(manifest: &str) -> Target {
    Target {
        id: format!("uv:{manifest}"),
        manager: "uv".into(),
        manifest: manifest.into(),
    }
}

#[test]
fn lock_owners_are_targets_and_members_are_not() {
    let root = tempfile::tempdir().unwrap();
    workspace(root.path());
    let ids: Vec<String> = Engine::default()
        .discover(root.path())
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .filter(|id| id.starts_with("uv:"))
        .collect();
    assert_eq!(
        ids,
        [
            "uv:packages/skip/pyproject.toml",
            "uv:pyproject.toml",
            "uv:tooled/pyproject.toml"
        ]
    );
}

#[test]
fn registry_requirements_of_the_workspace_are_declarations() {
    let root = tempfile::tempdir().unwrap();
    workspace(root.path());
    let declared: Vec<(String, String, String, String)> = Uv::default()
        .declarations(root.path(), &target("pyproject.toml"))
        .unwrap()
        .into_iter()
        .map(|d| {
            assert_eq!(d.ecosystem, "pypi");
            (
                d.file.to_string_lossy().into_owned(),
                d.location,
                d.package,
                d.requirement,
            )
        })
        .collect();
    let expected = [
        (
            "pyproject.toml",
            "project.dependencies[0]",
            "six",
            "==1.15.0",
        ),
        ("pyproject.toml", "project.dependencies[3]", "iniconfig", ""),
        (
            "pyproject.toml",
            "dependency-groups.dev[0]",
            "packaging",
            "<24",
        ),
        (
            "pyproject.toml",
            "tool.uv.dev-dependencies[0]",
            "pytest",
            ">=8",
        ),
        (
            "packages/member/pyproject.toml",
            "project.dependencies[0]",
            "idna",
            ">=3",
        ),
    ]
    .map(|(f, l, p, r)| (f.to_owned(), l.to_owned(), p.to_owned(), r.to_owned()));
    assert_eq!(declared, expected);

    let selected = Uv::default()
        .select(
            root.path(),
            &target("pyproject.toml"),
            &[
                "SIX".into(),
                "idna".into(),
                "member".into(),
                "absent".into(),
            ],
        )
        .unwrap();
    assert_eq!(
        selected.into_iter().collect::<Vec<_>>(),
        [
            ("SIX".to_owned(), "six".to_owned()),
            ("idna".to_owned(), "idna".to_owned()),
            ("member".to_owned(), "member".to_owned()),
        ]
    );
}

#[test]
fn rewrites_keep_comments_and_reach_members() {
    let root = tempfile::tempdir().unwrap();
    workspace(root.path());
    let uv = Uv::default();
    let declared = uv
        .declarations(root.path(), &target("pyproject.toml"))
        .unwrap();
    let edit = |package: &str, requirement: &str| depsmith_core::constraints::Edit {
        declaration: declared
            .iter()
            .find(|d| d.package == package)
            .unwrap()
            .clone(),
        requirement: requirement.into(),
        comment: None,
    };
    uv.rewrite(
        root.path(),
        &target("pyproject.toml"),
        &[edit("six", "==1.17.0"), edit("idna", ">=4")],
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(root.path().join("pyproject.toml")).unwrap(),
        ROOT.replace("six==1.15.0", "six==1.17.0")
    );
    assert!(
        fs::read_to_string(root.path().join("packages/member/pyproject.toml"))
            .unwrap()
            .contains("\"idna>=4\"")
    );
    let stale = uv.rewrite(
        root.path(),
        &target("pyproject.toml"),
        &[edit("six", "==1.18.0")],
    );
    assert!(stale.is_err(), "{stale:?}");
}

const LOCK: &str = r#"version = 1
revision = 3
requires-python = ">=3.10"

[[package]]
name = "root"
version = "0.1.0"
source = { virtual = "." }

[[package]]
name = "member"
version = "0.1.0"
source = { editable = "packages/member" }

[[package]]
name = "iniconfig"
version = "2.0.0"
source = { git = "https://github.com/pytest-dev/iniconfig?rev=v2.0.0#93f5930e668c0d1ddf4597e38dd0dea4e2665e7a" }

[[package]]
name = "six"
version = "1.15.0"
source = { registry = "https://pypi.org/simple" }
sdist = { url = "https://files.pythonhosted.org/packages/6b/34/six-1.15.0.tar.gz", hash = "sha256:30", size = 33917 }
wheels = [
    { url = "https://files.pythonhosted.org/packages/ee/ff/six-1.15.0-py2.py3-none-any.whl", hash = "sha256:8b", size = 10963 },
]

[[package]]
name = "wheel-only"
version = "1.0"
source = { registry = "https://mirror.example/simple" }
wheels = [{ url = "https://mirror.example/files/wheel_only-1.0-py3-none-any.whl", hash = "sha256:00" }]

[[package]]
name = "vendored"
version = "2.0"
source = { registry = "../wheels" }
wheels = [{ path = "vendored-2.0-py3-none-any.whl", hash = "sha256:01" }]
"#;

#[test]
fn lock_inventory_keeps_index_and_git_packages() {
    let packages: Vec<(String, String, String)> = depsmith_core::uv::lock_inventory(LOCK)
        .unwrap()
        .into_iter()
        .map(|p| {
            assert_eq!((p.ecosystem.as_str(), p.platform.as_str()), ("pypi", "any"));
            (p.name, p.version, p.artifact)
        })
        .collect();
    let expected = [
        (
            "iniconfig",
            "2.0.0",
            "git+https://github.com/pytest-dev/iniconfig?rev=v2.0.0#93f5930e668c0d1ddf4597e38dd0dea4e2665e7a",
        ),
        (
            "six",
            "1.15.0",
            "https://files.pythonhosted.org/packages/6b/34/six-1.15.0.tar.gz",
        ),
        (
            "wheel-only",
            "1.0",
            "https://mirror.example/files/wheel_only-1.0-py3-none-any.whl",
        ),
        ("vendored", "2.0", "../wheels/vendored-2.0-py3-none-any.whl"),
    ]
    .map(|(n, v, a)| (n.to_owned(), v.to_owned(), a.to_owned()));
    assert_eq!(packages, expected);
    let future = depsmith_core::uv::lock_inventory("version = 2\n");
    assert!(future.is_err(), "{future:?}");
}

#[test]
fn indexes_follow_uv_priority_and_uv_toml_wins() {
    use depsmith_core::constraints::RegistryConfig;
    let root = tempfile::tempdir().unwrap();
    let indexes = |root: &Path| {
        let config = Uv::default()
            .availability(root, &target("pyproject.toml"))
            .unwrap();
        let Some(RegistryConfig::PypiSimple { indexes }) = config.registries.get("pypi") else {
            panic!("{config:?}");
        };
        (indexes.clone(), config.exclude_newer)
    };
    write(
        root.path(),
        &[("pyproject.toml", &project("p", "[tool.uv]\n"))],
    );
    assert_eq!(
        indexes(root.path()),
        (vec!["https://pypi.org/simple".to_owned()], None)
    );
    write(
        root.path(),
        &[(
            "pyproject.toml",
            &project(
                "p",
                r#"[tool.uv]
exclude-newer = "2026-01-01T00:00:00Z"
extra-index-url = ["https://user:secret@extra.example/simple/"]

[[tool.uv.index]]
name = "mine"
url = "https://mine.example/simple"

[[tool.uv.index]]
name = "pinned-only"
url = "https://explicit.example/simple"
explicit = true

[[tool.uv.index]]
name = "corp"
url = "https://corp.example/simple"
default = true
"#,
            ),
        )],
    );
    assert_eq!(
        indexes(root.path()),
        (
            vec![
                "https://mine.example/simple".to_owned(),
                "https://extra.example/simple".to_owned(),
                "https://corp.example/simple".to_owned(),
            ],
            Some("2026-01-01T00:00:00Z".to_owned())
        )
    );
    write(
        root.path(),
        &[("uv.toml", "index-url = \"https://only.example/simple\"\n")],
    );
    assert_eq!(
        indexes(root.path()),
        (vec!["https://only.example/simple".to_owned()], None)
    );
}

#[test]
fn local_date_cutoffs_are_not_reinterpreted() {
    let root = tempfile::tempdir().unwrap();
    let cutoff = |value: &str| {
        write(
            root.path(),
            &[(
                "pyproject.toml",
                &project("p", &format!("[tool.uv]\nexclude-newer = \"{value}\"\n")),
            )],
        );
        Uv::default()
            .availability(root.path(), &target("pyproject.toml"))
            .unwrap()
            .exclude_newer
            .unwrap()
    };
    assert_eq!(cutoff("14 days"), "14 days");
    // uv reads a bare date in the local time zone; the engine must not
    // guess, so the value no longer parses as a date.
    let date = cutoff("2026-01-01");
    assert!(
        date.starts_with("2026-01-01 ") && date.contains("local"),
        "{date}"
    );
}

#[test]
fn uv_passes_the_conformance_suite() {
    let fixture = conformance::Fixture {
        files: vec![
            (
                "pyproject.toml".into(),
                project(
                    "root",
                    "dependencies = [\"Six==1.15.0\"] # keep\n\n[tool.uv.workspace]\nmembers = [\"a\"]\n",
                ),
            ),
            (
                "a/pyproject.toml".into(),
                project("a", "dependencies = [\"idna>=3\"]\n"),
            ),
        ],
        target: "uv:pyproject.toml".into(),
        declared: vec![("six".into(), "Six".into()), ("IDNA".into(), "idna".into())],
    };
    conformance::check(&|| Box::new(Uv::default()) as Box<dyn Adapter>, &fixture).unwrap();
}

#[cfg(unix)]
mod stand_in {
    use super::*;
    use depsmith_core::{
        constraints::{FixtureRelease, RegistryConfig},
        Proposal, UpdateOptions,
    };
    use std::{collections::BTreeMap, os::unix::fs::PermissionsExt};

    const PYPROJECT: &str = "[project]\nname = \"p\"\nversion = \"0.1.0\"\ndependencies = [\n    \"six==1.15.0\", # pinned\n    \"idna\",\n    \"iniconfig @ git+https://github.com/pytest-dev/iniconfig@main\",\n]\n\n[tool.uv]\n";

    fn lock(idna: &str, commit: &str) -> String {
        format!(
            "version = 1\nrevision = 3\n\n[[package]]\nname = \"p\"\nversion = \"0.1.0\"\nsource = {{ virtual = \".\" }}\n\n\
             [[package]]\nname = \"six\"\nversion = \"1.15.0\"\nsource = {{ registry = \"https://pypi.org/simple\" }}\nsdist = {{ url = \"https://files.pythonhosted.org/six-1.15.0.tar.gz\" }}\n\n\
             [[package]]\nname = \"idna\"\nversion = \"{idna}\"\nsource = {{ registry = \"https://pypi.org/simple\" }}\nsdist = {{ url = \"https://files.pythonhosted.org/idna-{idna}.tar.gz\" }}\n\n\
             [[package]]\nname = \"iniconfig\"\nversion = \"2.0.0\"\nsource = {{ git = \"https://github.com/pytest-dev/iniconfig?branch=main#{commit}\" }}\n"
        )
    }

    struct Run {
        proposal: Proposal,
        /// The arguments of every uv invocation, one line each.
        calls: Vec<String>,
    }

    /// Prepare with a uv that logs its arguments and writes `next` as the
    /// lock, from a repository whose baseline lock is `baseline`.
    fn prepare(baseline: Option<&str>, next: &str, options: UpdateOptions) -> Run {
        prepare_with(baseline, next, options, "", "")
    }

    /// Like [`prepare`], running the shell commands `after_lock` after
    /// writing the lock and `after_locked` for the `--locked` re-run.
    fn prepare_with(
        baseline: Option<&str>,
        next: &str,
        options: UpdateOptions,
        after_lock: &str,
        after_locked: &str,
    ) -> Run {
        let root = tempfile::tempdir().unwrap();
        let tools = tempfile::tempdir().unwrap();
        write(root.path(), &[("pyproject.toml", PYPROJECT)]);
        if let Some(baseline) = baseline {
            write(root.path(), &[("uv.lock", baseline)]);
        }
        let (script, log, next_lock) = (
            tools.path().join("uv"),
            tools.path().join("log"),
            tools.path().join("next.lock"),
        );
        fs::write(&next_lock, next).unwrap();
        fs::write(
            &script,
            format!(
                "#!/bin/sh\necho \"$*\" >> '{log}'\ncase \"$1\" in\n  --version) echo 'uv 0.12.15';;\n  lock) case \"$*\" in *--locked*) cmp -s '{next}' uv.lock || exit 1; {after_locked};; *) cp '{next}' uv.lock; {after_lock};; esac;;\n  *) exit 2;;\nesac\n",
                log = log.display(),
                next = next_lock.display(),
                after_lock = if after_lock.is_empty() { ":" } else { after_lock },
                after_locked = if after_locked.is_empty() { ":" } else { after_locked },
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let release = |version: &str| FixtureRelease {
            version: version.into(),
            url: format!("https://files.pythonhosted.org/six-{version}.tar.gz"),
            sha256: format!("sum-{version}"),
        };
        let engine = Engine::new(vec![Box::new(Uv {
            registry: Some(RegistryConfig::Fixture {
                releases: BTreeMap::from([(
                    "six".into(),
                    vec![release("1.15.0"), release("1.16.0"), release("1.17.0")],
                )]),
                failing: vec![],
            }),
        })]);
        let options = UpdateOptions {
            tools: BTreeMap::from([("uv".into(), script.to_string_lossy().into_owned())]),
            ..options
        };
        let proposal = engine
            .prepare(root.path(), &["uv:pyproject.toml".into()], options)
            .unwrap();
        let calls = fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect();
        Run { proposal, calls }
    }

    fn lock_calls(run: &Run) -> Vec<&str> {
        run.calls
            .iter()
            .filter(|c| c.starts_with("lock "))
            .map(String::as_str)
            .collect()
    }

    #[test]
    fn index_packages_upgrade_while_git_pins_stay() {
        let run = prepare(
            Some(&lock("3.0", "aaaa")),
            &lock("3.10", "aaaa"),
            UpdateOptions::default(),
        );
        assert!(
            run.proposal.failures.is_empty(),
            "{:?}",
            run.proposal.failures
        );
        let calls = lock_calls(&run);
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert!(
            calls[0].contains("--color never")
                && calls[0].contains("--upgrade-package six")
                && calls[0].contains("--upgrade-package idna")
                && !calls[0].contains("iniconfig")
                && !calls[0].contains("--upgrade "),
            "{}",
            calls[0]
        );
        assert!(calls[1].contains("--locked"), "{}", calls[1]);
        assert!(run
            .proposal
            .changes
            .iter()
            .any(|c| c.path == Path::new("uv.lock") && c.after.contains("idna-3.10")));
        let six = run
            .proposal
            .suggestions
            .iter()
            .find(|s| s.package == "six")
            .unwrap_or_else(|| panic!("{:?}", run.proposal.suggestions));
        assert_eq!(six.requirement, "==1.15.0");
        assert!(six.evidence[0].contains("1.17.0"), "{:?}", six.evidence);
    }

    #[test]
    fn without_git_pins_or_a_lock_everything_upgrades() {
        let next = lock("3.10", "aaaa").replace("iniconfig", "other").replace(
            "{ git = \"https://github.com/pytest-dev/other?branch=main#aaaa\" }",
            "{ registry = \"https://pypi.org/simple\" }",
        );
        let run = prepare(None, &next, UpdateOptions::default());
        assert!(
            run.proposal.failures.is_empty(),
            "{:?}",
            run.proposal.failures
        );
        assert!(
            lock_calls(&run)[0].ends_with(" --upgrade"),
            "{:?}",
            run.calls
        );
    }

    #[test]
    fn selected_packages_alone_upgrade() {
        let run = prepare(
            Some(&lock("3.0", "aaaa")),
            &lock("3.10", "aaaa"),
            UpdateOptions {
                packages: vec!["IDNA".into()],
                ..Default::default()
            },
        );
        assert!(
            run.proposal.failures.is_empty(),
            "{:?}",
            run.proposal.failures
        );
        let first = lock_calls(&run)[0];
        assert!(
            first.ends_with(" --upgrade-package idna") && !first.contains("six"),
            "{first}"
        );
    }

    #[test]
    fn moving_git_pins_needs_refresh_git() {
        let moved = prepare(
            Some(&lock("3.0", "aaaa")),
            &lock("3.0", "bbbb"),
            UpdateOptions::default(),
        );
        assert_eq!(moved.proposal.failures.len(), 1, "{:?}", moved.proposal);
        assert!(
            moved.proposal.failures[0].message.contains("--refresh-git"),
            "{}",
            moved.proposal.failures[0].message
        );
        let refreshed = prepare(
            Some(&lock("3.0", "aaaa")),
            &lock("3.0", "bbbb"),
            UpdateOptions {
                refresh_git: true,
                ..Default::default()
            },
        );
        assert!(
            refreshed.proposal.failures.is_empty(),
            "{:?}",
            refreshed.proposal.failures
        );
        assert!(lock_calls(&refreshed)[0].ends_with(" --upgrade"));
    }

    #[test]
    fn accepting_rewrites_the_requirement_before_locking() {
        let run = prepare(
            Some(&lock("3.0", "aaaa")),
            &lock("3.0", "aaaa"),
            UpdateOptions {
                accept: vec!["six".into()],
                ..Default::default()
            },
        );
        assert!(
            run.proposal.failures.is_empty(),
            "{:?}",
            run.proposal.failures
        );
        let manifest = run
            .proposal
            .changes
            .iter()
            .find(|c| c.path == Path::new("pyproject.toml"))
            .unwrap();
        assert!(
            manifest.after.contains("\"six==1.17.0\", # pinned"),
            "{}",
            manifest.after
        );
    }

    fn single_failure(run: &Run) -> &str {
        assert_eq!(run.proposal.failures.len(), 1, "{:?}", run.proposal);
        &run.proposal.failures[0].message
    }

    #[test]
    fn a_backend_changing_a_manifest_is_refused() {
        let run = prepare_with(
            Some(&lock("3.0", "aaaa")),
            &lock("3.10", "aaaa"),
            UpdateOptions::default(),
            "echo '# touched' >> pyproject.toml",
            "",
        );
        let message = single_failure(&run);
        assert!(
            message.contains("backend unexpectedly changed pyproject.toml"),
            "{message}"
        );
    }

    #[test]
    fn a_lock_that_changes_when_rechecked_is_refused() {
        let run = prepare_with(
            Some(&lock("3.0", "aaaa")),
            &lock("3.10", "aaaa"),
            UpdateOptions::default(),
            "",
            "echo '# again' >> uv.lock",
        );
        let message = single_failure(&run);
        assert!(
            message.contains("lock consistency check changed the candidate"),
            "{message}"
        );
    }
}

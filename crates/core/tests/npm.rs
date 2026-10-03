//! The npm adapter offline: lock-owner discovery, workspace declarations,
//! formatting-preserving rewrites, inventory, registry evidence, and
//! suggestions, `--accept`, Git pins and `--before` with a stand-in npm.
use depsmith_core::{
    adapter::Adapter,
    conformance,
    constraints::{Edit, RegistryConfig},
    npm::{lock_inventory, Npm},
    Engine, Target,
};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn write(root: &Path, files: &[(&str, &str)]) {
    for (path, content) in files {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
}

const ROOT: &str = r#"{
  "name": "root",
  "private": true,
  "workspaces": ["packages/*"],
  "dependencies": {
    "ms": "2.0.0",
    "debug": "^3.0.0",
    "member": "workspace:*",
    "local": "file:../local",
    "forked": "github:someone/forked"
  },
  "devDependencies": { "is-number": "~6.0.0" }
}
"#;

fn workspace(root: &Path) {
    write(
        root,
        &[
            ("package.json", ROOT),
            (
                "package-lock.json",
                "{\"lockfileVersion\": 3, \"packages\": {}}",
            ),
            (
                "packages/a/package.json",
                "{\"name\": \"a\", \"dependencies\": {\"left-pad\": \"^1.1.0\"}}\n",
            ),
            ("tools/t/package.json", "{\"name\": \"t\"}"),
            ("tools/t/package-lock.json", "{\"lockfileVersion\": 3}"),
            ("yarn-only/package.json", "{\"name\": \"y\"}"),
            ("yarn-only/yarn.lock", ""),
            ("no-lock/package.json", "{\"name\": \"n\"}"),
        ],
    );
}

fn target(manifest: &str) -> Target {
    Target {
        id: format!("npm:{manifest}"),
        manager: "npm".into(),
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
        .filter(|id| id.starts_with("npm:"))
        .collect();
    assert_eq!(ids, ["npm:package.json", "npm:tools/t/package.json"]);
}

#[test]
fn registry_ranges_of_the_workspace_are_declarations() {
    let root = tempfile::tempdir().unwrap();
    workspace(root.path());
    let declared: Vec<(PathBuf, String, String, String)> = Npm::default()
        .declarations(root.path(), &target("package.json"))
        .unwrap()
        .into_iter()
        .map(|d| {
            assert_eq!(d.ecosystem, "npm");
            (d.file, d.location, d.package, d.requirement)
        })
        .collect();
    let expected = [
        ("package.json", "dependencies.debug", "debug", "^3.0.0"),
        ("package.json", "dependencies.ms", "ms", "2.0.0"),
        (
            "package.json",
            "devDependencies.is-number",
            "is-number",
            "~6.0.0",
        ),
        (
            "packages/a/package.json",
            "dependencies.left-pad",
            "left-pad",
            "^1.1.0",
        ),
    ]
    .map(|(f, l, p, r)| (PathBuf::from(f), l.to_owned(), p.to_owned(), r.to_owned()));
    assert_eq!(declared, expected);
    let selected = Npm::default()
        .select(
            root.path(),
            &target("package.json"),
            &[
                "ms".into(),
                "left-pad".into(),
                "forked".into(),
                "absent".into(),
            ],
        )
        .unwrap();
    assert_eq!(
        selected.into_keys().collect::<Vec<_>>(),
        ["forked", "left-pad", "ms"]
    );
}

#[test]
fn workspace_globs_recurse_and_negate() {
    let root = tempfile::tempdir().unwrap();
    let member = |name: &str| {
        format!("{{\"name\": \"{name}\", \"dependencies\": {{\"{name}-dep\": \"1\"}}}}")
    };
    write(
        root.path(),
        &[
            (
                "package.json",
                "\u{feff}{\"workspaces\": [\"packages/**\", \"!packages/skip\"]}",
            ),
            (
                "package-lock.json",
                "{\"lockfileVersion\": 3, \"packages\": {}}",
            ),
            ("packages/a/package.json", &member("a")),
            ("packages/group/b/package.json", &member("b")),
            ("packages/skip/package.json", &member("skip")),
            ("packages/a/node_modules/x/package.json", &member("x")),
        ],
    );
    let ids: Vec<String> = Engine::default()
        .discover(root.path())
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .filter(|id| id.starts_with("npm:"))
        .collect();
    assert_eq!(ids, ["npm:package.json"]);
    let packages: Vec<String> = Npm::default()
        .declarations(root.path(), &target("package.json"))
        .unwrap()
        .into_iter()
        .map(|d| d.package)
        .collect();
    assert_eq!(packages, ["a-dep", "b-dep"]);
}

#[test]
fn rewrites_keep_formatting_and_refuse_stale_edits() {
    let root = tempfile::tempdir().unwrap();
    workspace(root.path());
    let npm = Npm::default();
    let declared = npm
        .declarations(root.path(), &target("package.json"))
        .unwrap();
    let edit = |package: &str, requirement: &str| Edit {
        declaration: declared
            .iter()
            .find(|d| d.package == package)
            .unwrap()
            .clone(),
        requirement: requirement.into(),
        comment: None,
    };
    npm.rewrite(
        root.path(),
        &target("package.json"),
        &[edit("ms", "2.1.3"), edit("left-pad", "^1.3.0")],
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(root.path().join("package.json")).unwrap(),
        ROOT.replace("\"ms\": \"2.0.0\"", "\"ms\": \"2.1.3\"")
    );
    assert!(
        fs::read_to_string(root.path().join("packages/a/package.json"))
            .unwrap()
            .contains("\"left-pad\": \"^1.3.0\"")
    );
    let stale = npm.rewrite(root.path(), &target("package.json"), &[edit("ms", "3.0.0")]);
    assert!(stale.is_err(), "{stale:?}");
}

const LOCK: &str = r#"{
  "name": "root",
  "lockfileVersion": 3,
  "requires": true,
  "packages": {
    "": { "name": "root", "workspaces": ["packages/*"] },
    "node_modules/a": { "resolved": "packages/a", "link": true },
    "node_modules/debug": {
      "version": "3.2.7",
      "resolved": "https://registry.npmjs.org/debug/-/debug-3.2.7.tgz",
      "integrity": "sha512-x"
    },
    "node_modules/debug/node_modules/ms": {
      "version": "2.1.3",
      "resolved": "https://registry.npmjs.org/ms/-/ms-2.1.3.tgz"
    },
    "node_modules/forked": {
      "version": "1.0.0",
      "resolved": "git+ssh://git@github.com/someone/forked.git#0123abc"
    },
    "node_modules/alias": {
      "name": "ms",
      "version": "2.0.0",
      "resolved": "https://registry.npmjs.org/ms/-/ms-2.0.0.tgz"
    },
    "packages/a": { "name": "a", "version": "1.0.0" }
  }
}"#;

#[test]
fn lock_inventory_keeps_resolved_packages_only() {
    let packages: Vec<(String, String, String)> = lock_inventory(LOCK)
        .unwrap()
        .into_iter()
        .map(|p| {
            assert_eq!((p.ecosystem.as_str(), p.platform.as_str()), ("npm", "any"));
            (p.name, p.version, p.artifact)
        })
        .collect();
    let expected = [
        (
            "ms",
            "2.0.0",
            "https://registry.npmjs.org/ms/-/ms-2.0.0.tgz",
        ),
        (
            "debug",
            "3.2.7",
            "https://registry.npmjs.org/debug/-/debug-3.2.7.tgz",
        ),
        (
            "ms",
            "2.1.3",
            "https://registry.npmjs.org/ms/-/ms-2.1.3.tgz",
        ),
        (
            "forked",
            "1.0.0",
            "git+ssh://git@github.com/someone/forked.git#0123abc",
        ),
    ]
    .map(|(n, v, a)| (n.to_owned(), v.to_owned(), a.to_owned()));
    assert_eq!(packages, expected);
    assert!(lock_inventory("{\"lockfileVersion\": 1}").is_err());
}

#[test]
fn the_projects_npmrc_registries_are_consulted() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        &[
            ("package.json", "{\"dependencies\": {\"ms\": \"2.0.0\"}}"),
            (
                "package-lock.json",
                "{\"lockfileVersion\": 3, \"packages\": {}}",
            ),
            (
                ".npmrc",
                "registry=https://user:secret@mirror.example/npm\n@corp:registry=https://corp.example/\n",
            ),
        ],
    );
    let config = Npm::default()
        .availability(root.path(), &target("package.json"))
        .unwrap();
    let Some(RegistryConfig::NpmRegistry { url, scopes, npmrc }) = config.registries.get("npm")
    else {
        panic!("{config:?}");
    };
    if std::env::var_os("npm_config_registry").is_none()
        && std::env::var_os("NPM_CONFIG_REGISTRY").is_none()
    {
        assert_eq!(url, "https://mirror.example/npm/");
    }
    assert_eq!(scopes["@corp"], "https://corp.example/");
    // Credentials are never read from the repository's .npmrc.
    assert!(!npmrc.contains(&root.path().join(".npmrc")), "{npmrc:?}");
}

#[test]
fn npm_passes_the_conformance_suite() {
    let fixture = conformance::Fixture {
        files: vec![
            (
                "package.json".into(),
                "{\n  \"name\": \"root\",\n  \"workspaces\": [\"a\"],\n  \"dependencies\": {\"ms\": \"2.0.0\"}\n}\n".into(),
            ),
            ("package-lock.json".into(), "{\"lockfileVersion\": 3, \"packages\": {}}".into()),
            ("a/package.json".into(), "{\"name\": \"a\", \"dependencies\": {\"debug\": \"^3.0.0\"}}".into()),
        ],
        target: "npm:package.json".into(),
        declared: vec![("ms".into(), "ms".into()), ("debug".into(), "debug".into())],
    };
    conformance::check(&|| Box::new(Npm::default()) as Box<dyn Adapter>, &fixture).unwrap();
}

#[cfg(unix)]
mod stand_in {
    use super::*;
    use depsmith_core::{constraints::FixtureRelease, Proposal, UpdateOptions};
    use std::{collections::BTreeMap, os::unix::fs::PermissionsExt};

    const PACKAGE: &str = "{\n  \"name\": \"p\",\n  \"dependencies\": {\n    \"ms\": \"2.0.0\",\n    \"debug\": \"^3.0.0\",\n    \"forked\": \"github:someone/forked\"\n  }\n}\n";

    fn lock(debug: &str, commit: &str) -> String {
        format!(
            "{{\"lockfileVersion\": 3, \"packages\": {{\"\": {{\"name\": \"p\"}}, \
             \"node_modules/ms\": {{\"version\": \"2.0.0\", \"resolved\": \"https://registry.npmjs.org/ms/-/ms-2.0.0.tgz\"}}, \
             \"node_modules/debug\": {{\"version\": \"{debug}\", \"resolved\": \"https://registry.npmjs.org/debug/-/debug-{debug}.tgz\"}}, \
             \"node_modules/forked\": {{\"version\": \"1.0.0\", \"resolved\": \"git+ssh://git@github.com/someone/forked.git#{commit}\"}}}}}}"
        )
    }

    struct Run {
        proposal: Proposal,
        calls: Vec<String>,
    }

    fn prepare_with(
        baseline: Option<&str>,
        next: &str,
        options: UpdateOptions,
        after_update: &str,
    ) -> Run {
        let root = tempfile::tempdir().unwrap();
        let tools = tempfile::tempdir().unwrap();
        write(root.path(), &[("package.json", PACKAGE)]);
        if let Some(baseline) = baseline {
            write(root.path(), &[("package-lock.json", baseline)]);
        }
        let (script, log, next_lock) = (
            tools.path().join("npm"),
            tools.path().join("log"),
            tools.path().join("next.json"),
        );
        fs::write(&next_lock, next).unwrap();
        fs::write(
            &script,
            format!(
                "#!/bin/sh\necho \"$*\" >> '{log}'\ncase \"$1\" in\n  --version) echo 11.19.0;;\n  update) cp '{next}' package-lock.json; {after};;\n  install) cmp -s '{next}' package-lock.json || exit 1;;\n  *) exit 2;;\nesac\n",
                log = log.display(),
                next = next_lock.display(),
                after = if after_update.is_empty() { ":" } else { after_update },
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let release = |version: &str| FixtureRelease {
            version: version.into(),
            url: format!("https://registry.npmjs.org/ms/-/ms-{version}.tgz"),
            sha256: format!("sha512-{version}"),
        };
        let engine = Engine::new(vec![Box::new(Npm {
            registry: Some(RegistryConfig::Fixture {
                releases: BTreeMap::from([("ms".into(), vec![release("2.0.0"), release("2.1.3")])]),
                failing: vec![],
            }),
        })]);
        let options = UpdateOptions {
            tools: BTreeMap::from([("npm".into(), script.to_string_lossy().into_owned())]),
            ..options
        };
        let proposal = engine
            .prepare(root.path(), &["npm:package.json".into()], options)
            .unwrap();
        let calls = fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect();
        Run { proposal, calls }
    }

    fn prepare(baseline: Option<&str>, next: &str, options: UpdateOptions) -> Run {
        prepare_with(baseline, next, options, "")
    }

    fn update_call(run: &Run) -> &str {
        run.calls.iter().find(|c| c.starts_with("update")).unwrap()
    }

    #[test]
    fn registry_packages_update_while_git_pins_stay() {
        let run = prepare(
            Some(&lock("3.1.0", "aaaa")),
            &lock("3.2.7", "aaaa"),
            UpdateOptions::default(),
        );
        assert!(
            run.proposal.failures.is_empty(),
            "{:?}",
            run.proposal.failures
        );
        let update = update_call(&run);
        assert!(
            update.starts_with("update debug ms ")
                && !update.contains("forked")
                && update.contains("--package-lock-only")
                && update.contains("--ignore-scripts"),
            "{update}"
        );
        assert!(
            run.calls
                .iter()
                .any(|c| c.starts_with("install --package-lock-only")),
            "{:?}",
            run.calls
        );
        let ms = run
            .proposal
            .suggestions
            .iter()
            .find(|s| s.package == "ms")
            .unwrap_or_else(|| panic!("{:?}", run.proposal.suggestions));
        assert_eq!(ms.requirement, "2.0.0");
        assert!(ms.reason.contains("--accept ms"), "{}", ms.reason);
        assert!(ms.evidence[0].contains("2.1.3"), "{:?}", ms.evidence);
    }

    #[test]
    fn moving_git_pins_needs_refresh_git() {
        let moved = prepare(
            Some(&lock("3.1.0", "aaaa")),
            &lock("3.1.0", "bbbb"),
            UpdateOptions::default(),
        );
        assert_eq!(moved.proposal.failures.len(), 1, "{:?}", moved.proposal);
        assert!(moved.proposal.failures[0].message.contains("--refresh-git"));
        let refreshed = prepare(
            Some(&lock("3.1.0", "aaaa")),
            &lock("3.1.0", "bbbb"),
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
        assert!(update_call(&refreshed).starts_with("update --package-lock-only"));
    }

    #[test]
    fn selection_and_cooldown_reach_npm() {
        let run = prepare(
            Some(&lock("3.1.0", "aaaa")),
            &lock("3.2.7", "aaaa"),
            UpdateOptions {
                packages: vec!["debug".into()],
                cooldown_days: Some(7),
                ..Default::default()
            },
        );
        assert!(
            run.proposal.failures.is_empty(),
            "{:?}",
            run.proposal.failures
        );
        let update = update_call(&run);
        assert!(
            update.starts_with("update debug --package-lock-only"),
            "{update}"
        );
        assert!(update.contains("--before=20"), "{update}");
    }

    #[test]
    fn accepting_rewrites_the_range_before_npm_runs() {
        let run = prepare(
            Some(&lock("3.1.0", "aaaa")),
            &lock("3.1.0", "aaaa"),
            UpdateOptions {
                accept: vec!["ms".into()],
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
            .find(|c| c.path == Path::new("package.json"))
            .unwrap();
        assert_eq!(
            manifest.after,
            PACKAGE.replace("\"ms\": \"2.0.0\"", "\"ms\": \"2.1.3\"")
        );
    }

    #[test]
    fn a_backend_changing_the_manifest_is_refused() {
        let run = prepare_with(
            Some(&lock("3.1.0", "aaaa")),
            &lock("3.2.7", "aaaa"),
            UpdateOptions::default(),
            "echo ' ' >> package.json",
        );
        assert_eq!(run.proposal.failures.len(), 1, "{:?}", run.proposal);
        assert!(run.proposal.failures[0]
            .message
            .contains("unexpectedly changed package.json"));
    }
}

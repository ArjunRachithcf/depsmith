//! The lock change between two resolutions and the packages it affects:
//! updates, additions and removals flagged major and direct, the reverse walk
//! from each change up to the declared packages it reaches, runtime barriers
//! and the interpreter, and the two sides of a comparison read from git.
use depsmith_core::{
    adapter::Adapter,
    constraints::Declaration,
    graph::{LockGraph, Node, Requirement},
    impact::{
        affected, lock_changes, sides, ChangeKind, Comparison, FullRun, Lock, LockChange,
        PYTHON_STDLIB_MAP, RUNTIME_BARRIERS,
    },
    pixi::Pixi,
    uv::Uv,
    Package, Target,
};
use std::{fs, path::Path, process::Command};

fn package(ecosystem: &str, name: &str, version: &str, platform: &str) -> Package {
    Package {
        ecosystem: ecosystem.into(),
        name: name.into(),
        version: version.into(),
        artifact: format!("https://example.org/{name}-{version}"),
        platform: platform.into(),
        environments: vec![],
    }
}

fn pypi(name: &str, version: &str) -> Package {
    package("pypi", name, version, "any")
}

fn conda(name: &str, version: &str) -> Package {
    package("conda", name, version, "linux-64")
}

fn declared(ecosystem: &str, name: &str) -> Declaration {
    Declaration {
        ecosystem: ecosystem.into(),
        package: name.into(),
        requirement: String::new(),
        file: "pyproject.toml".into(),
        location: "project.dependencies".into(),
    }
}

fn node(ecosystem: &str, platform: &str, name: &str, requires: &[&str]) -> Node {
    Node {
        ecosystem: ecosystem.into(),
        name: name.into(),
        version: None,
        platform: platform.into(),
        requires: requires
            .iter()
            .map(|r| Requirement {
                name: (*r).into(),
                spec: None,
                version: None,
            })
            .collect(),
    }
}

fn change<'a>(changes: &'a [LockChange], name: &str) -> &'a LockChange {
    changes
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no change of {name} in {changes:?}"))
}

fn barriers() -> Vec<&'static str> {
    RUNTIME_BARRIERS.to_vec()
}

#[test]
fn updates_additions_and_removals_are_flagged_major_and_direct() {
    let before = [
        pypi("Requests", "2.31.0"),
        pypi("urllib3", "1.26.18"),
        pypi("six", "1.16.0"),
        pypi("anyio", "0.9.1"),
        pypi("idna", "3.6"),
        pypi("same", "1.0"),
    ];
    let mut rebuilt = pypi("same", "1.0");
    rebuilt.artifact = "https://example.org/same-1.0-rebuilt".into();
    let after = [
        pypi("requests", "2.32.0"),
        pypi("urllib3", "2.2.0"),
        pypi("anyio", "0.10.0"),
        pypi("idna", "3.7"),
        pypi("charset-normalizer", "3.3.2"),
        rebuilt,
    ];
    let changes = lock_changes(&before, &after, &[declared("pypi", "requests")]);
    assert_eq!(changes.len(), 7, "{changes:?}");

    let requests = change(&changes, "requests");
    assert_eq!(requests.kind, ChangeKind::Updated);
    assert_eq!(
        (requests.before.as_deref(), requests.after.as_deref()),
        (Some("2.31.0"), Some("2.32.0"))
    );
    assert_eq!(
        (requests.ecosystem.as_str(), requests.platform.as_str()),
        ("pypi", "any")
    );
    assert!(requests.direct, "declared names compare normalized");
    assert!(!requests.major);
    assert_eq!(requests.environment, None);

    let urllib3 = change(&changes, "urllib3");
    assert!(urllib3.major && !urllib3.direct);
    // Below 1.0 the leading 0.x component is the major one.
    assert!(change(&changes, "anyio").major);
    assert!(!change(&changes, "idna").major);
    // A changed source at the same version is still an update.
    let same = change(&changes, "same");
    assert_eq!(same.kind, ChangeKind::Updated);
    assert_eq!(same.before, same.after);

    let six = change(&changes, "six");
    assert_eq!(six.kind, ChangeKind::Removed);
    assert_eq!(
        (six.before.as_deref(), six.after.as_deref()),
        (Some("1.16.0"), None)
    );
    let added = change(&changes, "charset-normalizer");
    assert_eq!(added.kind, ChangeKind::Added);
    assert_eq!(added.before, None);
    assert!(!added.major);
}

#[test]
fn major_uses_the_ecosystem_version_scheme() {
    // PEP 440 compares `2` and `2.0` as equal and keeps an epoch apart.
    let changes = lock_changes(
        &[pypi("a", "2"), pypi("b", "1.9"), pypi("c", "v1.0")],
        &[pypi("a", "2.0.1"), pypi("b", "1!1.9"), pypi("c", "v1.1")],
        &[],
    );
    assert!(!change(&changes, "a").major);
    assert!(change(&changes, "b").major);
    assert!(!change(&changes, "c").major);
    // Conda names are not normalized like PyPI's.
    let changes = lock_changes(
        &[conda("ruamel.yaml", "0.17")],
        &[conda("ruamel.yaml", "0.18")],
        &[declared("conda", "ruamel-yaml")],
    );
    assert!(change(&changes, "ruamel.yaml").major);
    assert!(!change(&changes, "ruamel.yaml").direct);
}

#[test]
fn an_unchanged_lock_has_no_changes_and_platforms_are_apart() {
    let before = [
        conda("numpy", "1.26.4"),
        package("conda", "numpy", "1.26.4", "osx-arm64"),
    ];
    assert!(lock_changes(&before, &before, &[]).is_empty());
    let after = [
        conda("numpy", "2.0.0"),
        package("conda", "numpy", "1.26.4", "osx-arm64"),
    ];
    let changes = lock_changes(&before, &after, &[]);
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].platform, "linux-64");
}

#[test]
fn an_environment_catching_up_to_another_is_an_update_never_a_removal() {
    // The inventory records one package per locked version, so two
    // environments at different versions lock both. When one catches up, the
    // older version disappears while the package stays.
    let before = [conda("numpy", "1.26.4"), conda("numpy", "2.0.0")];
    let after = [conda("numpy", "2.0.0")];
    let changes = lock_changes(&before, &after, &[]);
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0].kind, ChangeKind::Updated);
    assert_eq!(
        (changes[0].before.as_deref(), changes[0].after.as_deref()),
        (Some("1.26.4"), Some("2.0.0"))
    );
    assert!(changes[0].major);
    // And one environment moving ahead is an update from the version it left.
    let changes = lock_changes(&after, &before, &[]);
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0].kind, ChangeKind::Updated);
    assert_eq!(
        (changes[0].before.as_deref(), changes[0].after.as_deref()),
        (Some("2.0.0"), Some("1.26.4"))
    );
}

/// `numpy` at `version` on linux-64, locked by `environments`.
fn numpy(version: &str, environments: &[&str]) -> Package {
    Package {
        environments: environments.iter().map(|e| e.to_string()).collect(),
        ..conda("numpy", version)
    }
}

#[test]
fn each_environment_has_its_own_changes() {
    let before = [numpy("2.0.0", &["default"]), numpy("1.26.4", &["lint"])];
    // lint drops numpy while default keeps it: a removal, not an update.
    let changes = lock_changes(&before, &[numpy("2.0.0", &["default"])], &[]);
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0].kind, ChangeKind::Removed);
    assert_eq!(changes[0].environment.as_deref(), Some("lint"));
    assert_eq!(changes[0].before.as_deref(), Some("1.26.4"));
    // lint catches up to default's version: an update in lint alone.
    let changes = lock_changes(&before, &[numpy("2.0.0", &["default", "lint"])], &[]);
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0].kind, ChangeKind::Updated);
    assert_eq!(changes[0].environment.as_deref(), Some("lint"));
    assert_eq!(
        (changes[0].before.as_deref(), changes[0].after.as_deref()),
        (Some("1.26.4"), Some("2.0.0"))
    );
    // A new environment locking the same version adds it there.
    let changes = lock_changes(
        &[numpy("2.0.0", &["default"])],
        &[numpy("2.0.0", &["default", "test"])],
        &[],
    );
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0].kind, ChangeKind::Added);
    assert_eq!(changes[0].environment.as_deref(), Some("test"));
}

#[test]
fn locks_without_versions_compare_by_source() {
    // A lock without versions leaves the inventory's version empty.
    let mut before = conda("zlib", "");
    before.artifact = "https://example.org/zlib-1.2.13-0.conda".into();
    let mut after = conda("zlib", "");
    after.artifact = "https://example.org/zlib-1.3.1-0.conda".into();
    let changes = lock_changes(&[before], &[after], &[]);
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].kind, ChangeKind::Updated);
    assert_eq!(
        (changes[0].before.as_deref(), changes[0].after.as_deref()),
        (None, None)
    );
    assert!(!changes[0].major);
}

fn pixi_graph(requests_needs: &[&str]) -> LockGraph {
    let linux = "linux-64";
    LockGraph {
        nodes: vec![
            node("conda", linux, "app-cli", &["requests", "python"]),
            node("conda", linux, "requests", requests_needs),
            node("conda", linux, "urllib3", &["python", "openssl"]),
            node("conda", linux, "numpy", &["libstdcxx", "python", "libblas"]),
            node("conda", linux, "libblas", &["libgcc"]),
            node("conda", linux, "python", &["openssl", "libzlib", "libgcc"]),
            node("conda", linux, "openssl", &["libgcc"]),
            node("conda", linux, "libzlib", &["libgcc"]),
            node("conda", linux, "libstdcxx", &["libgcc"]),
            node("conda", linux, "libgcc", &[]),
            node("conda", "osx-arm64", "requests", &["urllib3"]),
            node("conda", "osx-arm64", "urllib3", &[]),
        ],
    }
}

fn lock(packages: &[Package], graph: LockGraph) -> Lock {
    Lock {
        inventory: packages.to_vec(),
        graph,
        ..Lock::default()
    }
}

#[test]
fn affected_packages_walk_up_from_each_change_to_what_declares_it() {
    let graph = pixi_graph(&["urllib3", "python"]);
    let before = lock(
        &[conda("urllib3", "1.26.18"), conda("app-cli", "1.0")],
        graph.clone(),
    );
    let after = lock(&[conda("urllib3", "2.2.0"), conda("app-cli", "1.0")], graph);
    let result = affected(
        &before,
        &after,
        &[declared("conda", "app-cli")],
        &barriers(),
    );
    assert_eq!(result.changes.len(), 1);
    assert!(result.full_run.is_empty());

    let names: Vec<_> = result.packages.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["app-cli", "requests", "urllib3"]);
    let app = &result.packages[0];
    assert!(app.declared && !app.changed);
    assert_eq!(app.platform, "linux-64");
    assert_eq!(app.reaches.len(), 1);
    assert_eq!(app.reaches[0].change, "urllib3");
    assert_eq!(app.reaches[0].path, ["app-cli", "requests", "urllib3"]);
    let urllib3 = &result.packages[2];
    assert!(urllib3.changed && !urllib3.declared);
    assert_eq!(urllib3.reaches[0].path, ["urllib3"]);
}

#[test]
fn runtime_barriers_and_the_interpreter_stop_the_walk() {
    let graph = pixi_graph(&["urllib3"]);
    // A C/C++ runtime that changes affects only itself.
    let before = lock(&[conda("libgcc", "13.2.0")], graph.clone());
    let after = lock(&[conda("libgcc", "14.1.0")], graph.clone());
    let result = affected(&before, &after, &[declared("conda", "numpy")], &barriers());
    let names: Vec<_> = result.packages.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["libgcc"]);
    // Without barriers, everything above it is affected.
    let result = affected(&before, &after, &[declared("conda", "numpy")], &[]);
    assert!(result
        .packages
        .iter()
        .any(|p| p.name == "numpy" && p.declared));

    // A native library of the interpreter reaches its standard-library
    // modules and the packages that require it directly, but never walks
    // through `python` to every package that requires the interpreter.
    let before = lock(&[conda("openssl", "3.2.0")], graph.clone());
    let after = lock(&[conda("openssl", "3.3.0")], graph.clone());
    let result = affected(
        &before,
        &after,
        &[declared("conda", "app-cli")],
        &barriers(),
    );
    let names: Vec<_> = result.packages.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["app-cli", "openssl", "requests", "urllib3"]);
    let openssl = result
        .packages
        .iter()
        .find(|p| p.name == "openssl")
        .unwrap();
    assert_eq!(openssl.stdlib, ["ssl", "hashlib"]);
    assert!(result.full_run.is_empty());

    // The interpreter itself is a full run.
    let before = lock(&[conda("python", "3.11.9")], graph.clone());
    let after = lock(&[conda("python", "3.12.4")], graph);
    let result = affected(
        &before,
        &after,
        &[declared("conda", "app-cli")],
        &barriers(),
    );
    assert_eq!(
        result.full_run,
        [FullRun::Interpreter {
            name: "python".into(),
            platform: "linux-64".into(),
            before: Some("3.11.9".into()),
            after: Some("3.12.4".into()),
        }]
    );
    let names: Vec<_> = result.packages.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["python"]);
}

#[test]
fn a_removed_package_is_walked_in_the_before_graph() {
    let before = lock(&[conda("urllib3", "2.2.0")], pixi_graph(&["urllib3"]));
    let after = lock(&[], pixi_graph(&[]));
    let result = affected(
        &before,
        &after,
        &[declared("conda", "app-cli")],
        &barriers(),
    );
    assert_eq!(result.changes[0].kind, ChangeKind::Removed);
    let app = result
        .packages
        .iter()
        .find(|p| p.name == "app-cli")
        .unwrap();
    assert_eq!(app.reaches[0].path, ["app-cli", "requests", "urllib3"]);
}

#[test]
fn only_dependents_of_the_changed_version_are_affected() {
    // npm locks `ms` twice: web's copy moves, cli keeps its own.
    let versioned = |name: &str, version: &str, requires: &[(&str, &str)]| Node {
        ecosystem: "npm".into(),
        name: name.into(),
        version: Some(version.into()),
        platform: "any".into(),
        requires: requires
            .iter()
            .map(|(name, version)| Requirement {
                name: (*name).into(),
                spec: None,
                version: Some((*version).into()),
            })
            .collect(),
    };
    let graph = |web_ms: &str| LockGraph {
        nodes: vec![
            versioned("web", "1.0.0", &[("ms", web_ms)]),
            versioned("cli", "1.0.0", &[("ms", "1.0.0")]),
            versioned("ms", web_ms, &[]),
            versioned("ms", "1.0.0", &[]),
        ],
    };
    let npm = |name: &str, version: &str| package("npm", name, version, "any");
    let before = lock(
        &[
            npm("web", "1.0.0"),
            npm("cli", "1.0.0"),
            npm("ms", "2.0.0"),
            npm("ms", "1.0.0"),
        ],
        graph("2.0.0"),
    );
    let after = lock(
        &[
            npm("web", "1.0.0"),
            npm("cli", "1.0.0"),
            npm("ms", "2.1.3"),
            npm("ms", "1.0.0"),
        ],
        graph("2.1.3"),
    );
    let result = affected(
        &before,
        &after,
        &[declared("npm", "web"), declared("npm", "cli")],
        &[],
    );
    let names: Vec<&str> = result.packages.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["ms", "web"], "{:?}", result.packages);
}

#[test]
fn a_compiler_or_extension_build_tool_change_is_a_full_run() {
    let before = lock(
        &[
            conda("gxx_linux-64", "13.2.0"),
            conda("cython", "3.0.10"),
            pypi("idna", "3.6"),
        ],
        LockGraph::default(),
    );
    let after = lock(
        &[
            conda("gxx_linux-64", "14.1.0"),
            conda("cython", "3.0.11"),
            pypi("idna", "3.7"),
        ],
        LockGraph::default(),
    );
    let result = affected(&before, &after, &[], &[]);
    let tools: Vec<&str> = result
        .full_run
        .iter()
        .filter_map(|reason| match reason {
            FullRun::BuildTool { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(tools, ["cython", "gxx_linux-64"], "{:?}", result.full_run);
}

#[test]
fn the_tables_name_the_runtimes_and_stdlib_modules() {
    for runtime in [
        "libgcc-ng",
        "libstdcxx",
        "vc14_runtime",
        "ucrt",
        "_openmp_mutex",
    ] {
        assert!(RUNTIME_BARRIERS.contains(&runtime), "{runtime}");
    }
    let modules = |library: &str| {
        PYTHON_STDLIB_MAP
            .iter()
            .find(|(name, _)| *name == library)
            .map(|(_, modules)| modules.to_vec())
    };
    assert_eq!(modules("libsqlite"), Some(vec!["sqlite3"]));
    assert_eq!(
        modules("libexpat"),
        Some(vec!["xml.parsers.expat", "pyexpat"])
    );
    assert_eq!(modules("liblzma"), Some(vec!["lzma"]));
}

// The two sides of a comparison, read from a throwaway git repository.

const PYPROJECT: &str =
    "[project]\nname = \"svc\"\nversion = \"0.1.0\"\ndependencies = [\"idna>=3\"]\n";

fn uv_lock(idna: &str) -> String {
    format!(
        "version = 1\nrevision = 3\n\n\
         [[package]]\nname = \"svc\"\nversion = \"0.1.0\"\nsource = {{ virtual = \".\" }}\n\
         dependencies = [{{ name = \"idna\" }}]\n\n\
         [[package]]\nname = \"idna\"\nversion = \"{idna}\"\n\
         source = {{ registry = \"https://pypi.org/simple\" }}\n\
         sdist = {{ url = \"https://files.pythonhosted.org/idna-{idna}.tar.gz\" }}\n"
    )
}

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(root)
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn commit(root: &Path, idna: &str, message: &str) {
    fs::write(root.join("uv.lock"), uv_lock(idna)).unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", message]);
}

/// A repository on `main` with idna 3.6 committed, then a feature branch
/// with 3.7 committed, 3.8 staged and 3.9 in the worktree.
fn repository() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.name", "Test"]);
    git(root, &["config", "user.email", "test@example.org"]);
    fs::write(root.join("pyproject.toml"), PYPROJECT).unwrap();
    commit(root, "3.6", "base");
    git(root, &["checkout", "-q", "-b", "feature"]);
    commit(root, "3.7", "feature");
    fs::write(root.join("uv.lock"), uv_lock("3.8")).unwrap();
    git(root, &["add", "uv.lock"]);
    fs::write(root.join("uv.lock"), uv_lock("3.9")).unwrap();
    repo
}

fn target() -> Target {
    Target {
        id: "uv:pyproject.toml".into(),
        manager: "uv".into(),
        manifest: "pyproject.toml".into(),
    }
}

/// The idna version each side of `comparison` locks.
fn versions(root: &Path, comparison: Comparison) -> (String, String) {
    let sides = sides(root, &Uv::default(), &target(), &comparison).unwrap();
    let idna = |side: &Path| {
        Uv::default()
            .inventory(side, &target())
            .unwrap()
            .into_iter()
            .find(|p| p.name == "idna")
            .unwrap()
            .version
    };
    (idna(sides.before()), idna(sides.after()))
}

#[test]
fn each_comparison_reads_its_two_sides() {
    let repo = repository();
    let root = repo.path();
    let pair = |a: &str, b: &str| (a.to_owned(), b.to_owned());
    assert_eq!(versions(root, Comparison::Staged), pair("3.7", "3.8"));
    assert_eq!(versions(root, Comparison::Unstaged), pair("3.8", "3.9"));
    assert_eq!(
        versions(root, Comparison::Worktree { base: None }),
        pair("3.6", "3.9"),
        "the merge-base with main"
    );
    assert_eq!(
        versions(
            root,
            Comparison::Worktree {
                base: Some("HEAD".into())
            }
        ),
        pair("3.7", "3.9")
    );
    assert_eq!(
        versions(
            root,
            Comparison::Revisions {
                base: "main".into(),
                head: "feature".into()
            }
        ),
        pair("3.6", "3.7")
    );
    let old = root.join("old.lock");
    fs::write(&old, uv_lock("3.5")).unwrap();
    assert_eq!(
        versions(root, Comparison::OldLockfile(old)),
        pair("3.5", "3.9")
    );
}

#[test]
fn the_after_side_has_the_manifests_and_the_worktree_is_read_in_place() {
    let repo = repository();
    let root = repo.path();
    let staged = sides(root, &Uv::default(), &target(), &Comparison::Staged).unwrap();
    assert_eq!(
        fs::read_to_string(staged.after().join("pyproject.toml")).unwrap(),
        PYPROJECT
    );
    assert!(!Uv::default()
        .declarations(staged.after(), &target())
        .unwrap()
        .is_empty());
    let worktree = sides(root, &Uv::default(), &target(), &Comparison::Unstaged).unwrap();
    assert_eq!(worktree.after(), root);
}

#[test]
fn the_sources_of_a_git_after_side_are_its_files_as_git_stores_them() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.name", "Test"]);
    git(root, &["config", "user.email", "test@example.org"]);
    // A smudge filter and export attributes must not change what is read.
    git(root, &["config", "filter.shout.smudge", "tr a-z A-Z"]);
    let write = |path: &str, text: &str| {
        fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
        fs::write(root.join(path), text).unwrap();
    };
    write(".gitignore", "*.generated.py\n");
    write(
        "svc/.gitattributes",
        "tests/ export-ignore\n*.py filter=shout export-subst\n",
    );
    write("svc/pyproject.toml", PYPROJECT);
    write("svc/uv.lock", &uv_lock("3.6"));
    write("svc/src/svc/app.py", "import idna\n");
    write("web/index.js", "require('left-pad')\n");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "base"]);
    write("svc/src/svc/app.py", "import idna.core  # $Format:%H$\n");
    write("svc/tests/test_app.py", "import svc.app\n");
    git(root, &["add", "."]);
    write("svc/src/svc/app.py", "import idna.codec\n");
    let target = Target {
        id: "uv:svc/pyproject.toml".into(),
        manager: "uv".into(),
        manifest: "svc/pyproject.toml".into(),
    };
    let read = |side: &Path, path: &str| fs::read_to_string(side.join(path)).ok();
    let app = "import idna.core  # $Format:%H$\n";

    let mut staged = sides(root, &Uv::default(), &target, &Comparison::Staged).unwrap();
    assert!(
        read(staged.after(), "svc/src/svc/app.py").is_none(),
        "sources are read only when asked for"
    );
    let after = staged.sources().unwrap().to_owned();
    assert_eq!(after, staged.after());
    assert_eq!(
        read(&after, "svc/src/svc/app.py").as_deref(),
        Some(app),
        "the index, not the worktree, unfiltered"
    );
    assert!(read(&after, "svc/tests/test_app.py").is_some());
    assert!(read(&after, "svc/uv.lock").is_some());
    assert!(
        read(&after, ".gitignore").is_some(),
        "ignore files above the target still apply"
    );
    assert!(
        read(&after, "web/index.js").is_none(),
        "only the target's directory"
    );
    assert!(
        read(staged.before(), "svc/src/svc/app.py").is_none(),
        "the before side holds only the manifest and its inputs"
    );

    git(root, &["commit", "-q", "-m", "feature"]);
    let mut revisions = sides(
        root,
        &Uv::default(),
        &target,
        &Comparison::Revisions {
            base: "HEAD~1".into(),
            head: "HEAD".into(),
        },
    )
    .unwrap();
    let after = revisions.sources().unwrap().to_owned();
    assert_eq!(read(&after, "svc/src/svc/app.py").as_deref(), Some(app));
    assert!(
        read(&after, "svc/tests/test_app.py").is_some(),
        "export-ignore does not apply"
    );
    assert!(read(&after, "web/index.js").is_none());
    assert!(read(revisions.before(), "svc/src/svc/app.py").is_none());

    let mut worktree = sides(root, &Uv::default(), &target, &Comparison::Unstaged).unwrap();
    assert_eq!(worktree.sources().unwrap(), root);
}

#[test]
fn a_lock_missing_on_one_side_reads_as_no_lock() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.name", "Test"]);
    git(root, &["config", "user.email", "test@example.org"]);
    fs::write(root.join("pyproject.toml"), PYPROJECT).unwrap();
    git(root, &["add", "pyproject.toml"]);
    git(root, &["commit", "-q", "-m", "no lock yet"]);
    fs::write(root.join("uv.lock"), uv_lock("3.7")).unwrap();
    git(root, &["add", "uv.lock"]);
    let sides = sides(root, &Uv::default(), &target(), &Comparison::Staged).unwrap();
    assert!(!sides.before().join("uv.lock").exists());
    let before = Lock::read(&Uv::default(), sides.before(), &target()).unwrap();
    let after = Lock::read(&Uv::default(), sides.after(), &target()).unwrap();
    assert_eq!(before, Lock::default());
    // Locking for the first time adds every package.
    let changes = lock_changes(&before.inventory, &after.inventory, &[]);
    assert!(
        !changes.is_empty() && changes.iter().all(|c| c.kind == ChangeKind::Added),
        "{changes:?}"
    );
}

#[test]
fn git_failures_are_invalid_requests() {
    let repo = repository();
    let root = repo.path();
    let error = sides(
        root,
        &Uv::default(),
        &target(),
        &Comparison::Worktree {
            base: Some("no-such-ref".into()),
        },
    )
    .err()
    .unwrap();
    assert_eq!(error.exit_code(), 2, "{error}");
    assert!(error.to_string().contains("no-such-ref"), "{error}");

    // Without a default branch there is no merge-base to compare with.
    git(root, &["branch", "-q", "-m", "main", "trunk"]);
    let error = sides(
        root,
        &Uv::default(),
        &target(),
        &Comparison::Worktree { base: None },
    )
    .err()
    .unwrap();
    assert_eq!(error.exit_code(), 2, "{error}");
    assert!(error.to_string().contains("--base"), "{error}");

    let plain = tempfile::tempdir().unwrap();
    let error = sides(plain.path(), &Uv::default(), &target(), &Comparison::Staged)
        .err()
        .unwrap();
    assert_eq!(error.exit_code(), 2, "{error}");
}

#[test]
fn the_default_comparison_follows_ci() {
    assert_eq!(Comparison::default_for(false), Comparison::Staged);
    assert_eq!(
        Comparison::default_for(true),
        Comparison::Worktree { base: None }
    );
    assert_eq!(
        Comparison::default_for_env(),
        Comparison::default_for(std::env::var_os("CI").is_some())
    );
}

/// A Pixi v7 lock whose two named platforms are `linux` (linux-64 with
/// `cuda`) and `windows` (win-64 with `cuda`), locking `zlib` at
/// `zlib_version` and `pillow` 11, which requires it.
fn named_platform_lock(linux: (&str, &str), windows: (&str, &str), zlib_version: &str) -> String {
    let platform = |(name, cuda): (&str, &str), subdir: &str| {
        format!("- name: {name}\n  subdir: {subdir}\n  virtual-packages:\n  - __cuda={cuda}\n  - __glibc=2.28\n")
    };
    let zlib = |subdir: &str| {
        format!("https://conda.anaconda.org/conda-forge/{subdir}/zlib-{zlib_version}-h0_0.conda")
    };
    let pillow = |subdir: &str| {
        format!("https://conda.anaconda.org/conda-forge/{subdir}/pillow-11.0.0-h0_0.conda")
    };
    format!(
        "version: 7\nplatforms:\n{}{}environments:\n  default:\n    packages:\n      {}:\n      - conda: {}\n      - conda: {}\n      {}:\n      - conda: {}\n      - conda: {}\npackages:\n- conda: {}\n- conda: {}\n  depends:\n  - zlib\n- conda: {}\n- conda: {}\n  depends:\n  - zlib\n",
        platform(linux, "linux-64"),
        platform(windows, "win-64"),
        linux.0,
        zlib("linux-64"),
        pillow("linux-64"),
        windows.0,
        zlib("win-64"),
        pillow("win-64"),
        zlib("linux-64"),
        pillow("linux-64"),
        zlib("win-64"),
        pillow("win-64"),
    )
}

fn pixi_side(lock: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("pixi.toml"),
        "[workspace]\nname = \"svc\"\n",
    )
    .unwrap();
    fs::write(dir.path().join("pixi.lock"), lock).unwrap();
    dir
}

#[test]
fn renamed_pixi_platforms_pair_by_what_they_are_not_their_names() {
    let target = Target {
        id: "pixi:pixi.toml".into(),
        manager: "pixi".into(),
        manifest: "pixi.toml".into(),
    };
    let read = |lock: &str| {
        let dir = pixi_side(lock);
        Lock::read(&Pixi, dir.path(), &target).unwrap()
    };
    // Renamed, with zlib bumped: the same subdir and virtual packages.
    let before = read(&named_platform_lock(("a", "12"), ("b", "12"), "1.3.0"));
    let after = read(&named_platform_lock(("c", "12"), ("d", "12"), "1.3.1"));
    let result = affected(&before, &after, &[], &barriers());
    let changes: Vec<_> = result
        .changes
        .iter()
        .map(|c| (c.name.as_str(), c.platform.as_str()))
        .collect();
    assert_eq!(
        changes,
        [("zlib", "c"), ("zlib", "d")],
        "named by the after side"
    );
    assert!(result.full_run.is_empty(), "{:?}", result.full_run);
    let pillow: Vec<_> = result
        .packages
        .iter()
        .filter(|p| p.name == "pillow")
        .map(|p| p.platform.as_str())
        .collect();
    assert_eq!(pillow, ["c", "d"], "the graph is renamed too");

    // Renamed along with a CUDA bump: the only platform of its subdir.
    let bumped = read(&named_platform_lock(("c", "13"), ("d", "13"), "1.3.0"));
    let result = affected(&before, &bumped, &[], &barriers());
    assert!(result.changes.is_empty(), "{:?}", result.changes);
    let virtual_packages = |platform: &str, cuda: &str| FullRun::VirtualPackages {
        platform: platform.into(),
        before: vec!["__cuda=12".into(), "__glibc=2.28".into()],
        after: vec![format!("__cuda={cuda}"), "__glibc=2.28".into()],
    };
    assert_eq!(
        result.full_run,
        [virtual_packages("c", "13"), virtual_packages("d", "13")],
        "a changed requirement of the platform is a full run"
    );
    // The same names, with only the CUDA requirement raised.
    let raised = read(&named_platform_lock(("a", "13"), ("b", "13"), "1.3.0"));
    let result = affected(&before, &raised, &[], &barriers());
    assert!(result.changes.is_empty(), "{:?}", result.changes);
    assert_eq!(
        result.full_run,
        [virtual_packages("a", "13"), virtual_packages("b", "13")]
    );
}

#[test]
fn swapped_and_chained_platform_names_still_pair_by_what_they_are() {
    let target = Target {
        id: "pixi:pixi.toml".into(),
        manager: "pixi".into(),
        manifest: "pixi.toml".into(),
    };
    let read = |lock: &str| {
        let dir = pixi_side(lock);
        Lock::read(&Pixi, dir.path(), &target).unwrap()
    };
    let before = read(&named_platform_lock(("a", "12"), ("b", "12"), "1.3.0"));
    // `a` and `b` swap places.
    let swapped = read(&named_platform_lock(("b", "12"), ("a", "12"), "1.3.0"));
    let result = affected(&before, &swapped, &[], &barriers());
    assert!(result.changes.is_empty(), "{:?}", result.changes);
    assert!(result.full_run.is_empty(), "{:?}", result.full_run);
    // linux-64 moves from `a` to `b` while win-64 moves from `b` to `c`.
    let chained = read(&named_platform_lock(("b", "12"), ("c", "12"), "1.3.1"));
    let result = affected(&before, &chained, &[], &barriers());
    let changes: Vec<_> = result
        .changes
        .iter()
        .map(|c| (c.name.as_str(), c.platform.as_str()))
        .collect();
    assert_eq!(changes, [("zlib", "b"), ("zlib", "c")]);
    assert!(result.full_run.is_empty(), "{:?}", result.full_run);
}

#[test]
fn a_lock_moving_to_named_platforms_pairs_them_by_subdir() {
    let target = Target {
        id: "pixi:pixi.toml".into(),
        manager: "pixi".into(),
        manifest: "pixi.toml".into(),
    };
    let read = |lock: &str| {
        let dir = pixi_side(lock);
        Lock::read(&Pixi, dir.path(), &target).unwrap()
    };
    let url = |name: &str, version: &str| {
        format!("https://conda.anaconda.org/conda-forge/linux-64/{name}-{version}-h0_0.conda")
    };
    // Schema 6 locks under the subdir; schema 7 under a platform name.
    let before = read(&format!(
        "version: 6\nenvironments:\n  default:\n    packages:\n      linux-64:\n      - conda: {zlib}\n      - conda: {pillow}\npackages:\n- conda: {zlib}\n- conda: {pillow}\n  depends:\n  - zlib\n",
        zlib = url("zlib", "1.3.0"),
        pillow = url("pillow", "11.0.0"),
    ));
    let after = read(&format!(
        "version: 7\nplatforms:\n- name: linux-64-glibc-2-17\n  subdir: linux-64\n  virtual-packages:\n  - __glibc=2.17\nenvironments:\n  default:\n    packages:\n      linux-64-glibc-2-17:\n      - conda: {zlib}\n      - conda: {pillow}\npackages:\n- conda: {zlib}\n- conda: {pillow}\n  depends:\n  - zlib\n",
        zlib = url("zlib", "1.3.1"),
        pillow = url("pillow", "11.0.0"),
    ));
    let result = affected(&before, &after, &[], &barriers());
    let changes: Vec<_> = result
        .changes
        .iter()
        .map(|c| (c.name.as_str(), c.platform.as_str()))
        .collect();
    assert_eq!(changes, [("zlib", "linux-64-glibc-2-17")]);
    // Schema 6 records no virtual packages, so none can have changed.
    assert!(result.full_run.is_empty(), "{:?}", result.full_run);
}

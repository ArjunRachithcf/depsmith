use depsmith_core::{Engine, Error, UpdateOptions};
use std::fs;
use tempfile::tempdir;

fn options(packages: &[&str]) -> UpdateOptions {
    UpdateOptions {
        pixi: "depsmith-nonexistent-executable".into(),
        packages: packages.iter().map(|p| p.to_string()).collect(),
        ..Default::default()
    }
}

fn write(root: &std::path::Path, path: &str, content: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

const PIXI: &str = "[workspace]\nname='a'\nchannels=['conda-forge']\nplatforms=['linux-64']\n";

#[test]
fn unknown_package_is_rejected_before_any_backend_runs() {
    let root = tempdir().unwrap();
    write(
        root.path(),
        "pixi.toml",
        &format!("{PIXI}[dependencies]\nruff='==0.15.22'\n"),
    );
    let error = Engine::default()
        .prepare(root.path(), &["pixi:pixi.toml".into()], options(&["ruf"]))
        .unwrap_err();
    assert!(
        matches!(&error, Error::Invalid(m) if m.contains("ruf") && m.contains("direct dependency")),
        "{error}"
    );
}

#[test]
fn feature_and_target_tables_count_as_direct_dependencies() {
    let root = tempdir().unwrap();
    write(
        root.path(),
        "pixi.toml",
        &format!("{PIXI}[feature.lint.dependencies]\nruff='*'\n[target.linux-64.pypi-dependencies]\nFoo_Bar='*'\n"),
    );
    let proposal = Engine::default()
        .prepare(
            root.path(),
            &["pixi:pixi.toml".into()],
            options(&["ruff", "foo-bar"]),
        )
        .unwrap();
    // Selection accepted; the missing backend is the only failure.
    assert_eq!(proposal.failures.len(), 1);
}

#[test]
fn pyproject_project_dependencies_are_selectable_by_normalized_name() {
    let root = tempdir().unwrap();
    write(
        root.path(),
        "pyproject.toml",
        "[project]\nname='demo'\ndependencies=['Requests[socks]>=2; python_version>\"3\"']\n[project.optional-dependencies]\ntest=['py.test']\n[dependency-groups]\ndev=['black', {include-group='x'}]\n[tool.pixi.workspace]\nchannels=['conda-forge']\nplatforms=['linux-64']\n",
    );
    let proposal = Engine::default()
        .prepare(
            root.path(),
            &["pixi:pyproject.toml".into()],
            options(&["requests", "py-test", "black"]),
        )
        .unwrap();
    assert_eq!(proposal.failures.len(), 1);
}

#[test]
fn targets_without_selected_packages_are_skipped_not_fully_updated() {
    let root = tempdir().unwrap();
    write(
        root.path(),
        "a/pixi.toml",
        &format!("{PIXI}[dependencies]\nruff='*'\n"),
    );
    write(
        root.path(),
        "b/pixi.toml",
        &format!("{PIXI}[dependencies]\nnumpy='*'\n"),
    );
    let proposal = Engine::default()
        .prepare(
            root.path(),
            &["pixi:a/pixi.toml".into(), "pixi:b/pixi.toml".into()],
            options(&["ruff"]),
        )
        .unwrap();
    assert_eq!(proposal.failures.len(), 1);
    assert_eq!(proposal.failures[0].target, "pixi:a/pixi.toml");
    assert!(
        proposal
            .validation
            .iter()
            .any(|v| v.starts_with("pixi:b/pixi.toml: skipped")),
        "{:?}",
        proposal.validation
    );
}

#[test]
fn unknown_action_repository_is_rejected() {
    let root = tempdir().unwrap();
    write(
        root.path(),
        ".github/workflows/ci.yml",
        "on: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n",
    );
    let error = Engine::default()
        .prepare(
            root.path(),
            &["github-actions:.github/workflows/ci.yml".into()],
            options(&["actions/chekout"]),
        )
        .unwrap_err();
    assert!(
        matches!(&error, Error::Invalid(m) if m.contains("actions/chekout")),
        "{error}"
    );
}

#[test]
fn conda_names_do_not_use_pypi_normalization() {
    let root = tempdir().unwrap();
    write(
        root.path(),
        "pixi.toml",
        &format!("{PIXI}[target.linux-64.dependencies]\nsysroot_linux-64='<=2.17'\n"),
    );
    let error = Engine::default()
        .prepare(
            root.path(),
            &["pixi:pixi.toml".into()],
            options(&["sysroot-linux-64"]),
        )
        .unwrap_err();
    assert!(
        matches!(&error, Error::Invalid(m) if m.contains("sysroot-linux-64")),
        "{error}"
    );
    let proposal = Engine::default()
        .prepare(
            root.path(),
            &["pixi:pixi.toml".into()],
            options(&["SYSROOT_linux-64"]),
        )
        .unwrap();
    assert_eq!(proposal.failures.len(), 1);
}

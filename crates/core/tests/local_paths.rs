//! Local path dependencies must stay inside the repository when staged.
use depsmith_core::{Engine, UpdateOptions};
use std::fs;
use tempfile::tempdir;

#[test]
fn pep508_file_urls_are_rejected_before_backend_execution() {
    let root = tempdir().unwrap();
    fs::write(root.path().join("pyproject.toml"), "[project]\nname='demo'\ndependencies=['other @ file:///tmp/external']\n[tool.pixi.workspace]\nchannels=[]\nplatforms=['linux-64']\n").unwrap();
    let options = UpdateOptions {
        pixi: "missing-backend".into(),
        ..Default::default()
    };
    let proposal = Engine::default()
        .prepare(root.path(), &["pixi:pyproject.toml".into()], options)
        .unwrap();
    assert_eq!(
        proposal.failures[0].code, 2,
        "external paths must be rejected before any tool execution"
    );
}
#[test]
fn nested_local_manifest_cannot_escape_the_stage() {
    let root = tempdir().unwrap();
    fs::create_dir(root.path().join("local")).unwrap();
    fs::write(
        root.path().join("pixi.toml"),
        "[workspace]\nname='demo'\n[pypi-dependencies]\nlocal={path='local'}\n",
    )
    .unwrap();
    fs::write(
        root.path().join("local/pyproject.toml"),
        "[project]\nname='local'\ndependencies=['other @ file:///tmp/external']\n",
    )
    .unwrap();
    let options = UpdateOptions {
        pixi: "missing-backend".into(),
        ..Default::default()
    };
    let proposal = Engine::default()
        .prepare(root.path(), &["pixi:pixi.toml".into()], options)
        .unwrap();
    assert_eq!(proposal.failures[0].code, 2);
}

const PIXI: &str = "[workspace]\nname='a'\nchannels=['conda-forge']\nplatforms=['linux-64']\n";

/// The failure message of the only target, with pixi itself unavailable.
fn failure(target: &str, files: &[(&str, &str)]) -> String {
    let root = tempdir().unwrap();
    for (path, text) in files {
        let path = root.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    let options = UpdateOptions {
        pixi: "depsmith-nonexistent-executable".into(),
        ..Default::default()
    };
    let proposal = Engine::default()
        .prepare(root.path(), &[target.into()], options)
        .unwrap();
    proposal.failures[0].message.clone()
}

/// Other tools' tables hold `path` keys that are not dependencies, such as
/// a hatch version file that is ignored and so never staged.
#[test]
fn other_tools_paths_are_not_dependency_paths() {
    let hatch = "[project]\nname='ext'\nversion='1'\n\
                 [tool.hatch.build.hooks.version]\npath = 'ext/_version.py'\n";
    let depends = format!("{PIXI}[pypi-dependencies]\next = {{ path = 'ext' }}\n");
    let tool = format!("{PIXI}[tool.other]\npath = 'generated/out.txt'\n");
    for files in [
        vec![("pixi.toml", PIXI), ("tools/ext/pyproject.toml", hatch)],
        vec![("pixi.toml", &*depends), ("ext/pyproject.toml", hatch)],
        vec![("pixi.toml", &*tool)],
    ] {
        let message = failure("pixi:pixi.toml", &files);
        assert!(!message.contains("unavailable in stage"), "{message}");
    }
}

#[test]
fn missing_or_escaping_dependency_paths_are_still_reported() {
    let pyproject = |body: &str| {
        format!("[project]\nname='a'\nversion='1'\n{body}\n[tool.pixi.workspace]\nchannels=[]\nplatforms=['linux-64']\n")
    };
    for (target, manifest, needle) in [
        (
            "pixi:pixi.toml",
            format!("{PIXI}[pypi-dependencies]\ngone = {{ path = 'gone' }}\n"),
            "local path unavailable in stage: gone",
        ),
        (
            "pixi:pixi.toml",
            format!("{PIXI}[pypi-dependencies]\nout = {{ path = '..' }}\n"),
            "escapes the repository",
        ),
        (
            "pixi:pyproject.toml",
            pyproject("[tool.pixi.pypi-dependencies]\ngone = { path = 'gone' }"),
            "local path unavailable in stage: gone",
        ),
        (
            "pixi:pyproject.toml",
            pyproject("dependencies = ['gone @ ./gone']"),
            "dependency reference is unavailable in stage",
        ),
        (
            "pixi:pyproject.toml",
            pyproject("[dependency-groups]\ndev = ['gone @ ./gone']"),
            "dependency reference is unavailable in stage",
        ),
    ] {
        let manifest_path = target.split_once(':').unwrap().1;
        let message = failure(target, &[(manifest_path, &manifest)]);
        assert!(message.contains(needle), "{target} {manifest}: {message}");
    }
}

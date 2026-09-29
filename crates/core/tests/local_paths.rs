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

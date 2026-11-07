//! Which pixi manifests are targets.
use depsmith_core::Engine;
use std::fs;
use tempfile::tempdir;

/// A pixi-build package manifest has no lock of its own: Pixi locks it
/// with the workspace above it. A manifest that does not parse stays a
/// target so its error is reported.
#[test]
fn package_manifests_are_not_targets() {
    let root = tempdir().unwrap();
    let workspace = "[workspace]\nname='w'\nchannels=['conda-forge']\nplatforms=['linux-64']\npreview=['pixi-build']\n";
    let package = "[package]\nname='p'\nversion='0.1.0'\n[package.build]\nbackend = { name = 'pixi-build-cmake', version = '0.3.*' }\n";
    for (path, text) in [
        ("pixi.toml", workspace),
        ("libs/core/pixi.toml", package),
        ("libs/both/pixi.toml", &*format!("{workspace}{package}")),
        ("legacy/pixi.toml", "[project]\nname='l'\nchannels=[]\nplatforms=['linux-64']\n"),
        ("py/pyproject.toml", "[project]\nname='py'\nversion='1'\n[tool.pixi.package.build]\nbackend = { name = 'pixi-build-python', version = '0.4.*' }\n"),
        ("broken/pixi.toml", "[workspace\n"),
    ] {
        let path = root.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    let ids: Vec<String> = Engine::default()
        .discover(root.path())
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(
        ids,
        [
            "pixi:broken/pixi.toml",
            "pixi:legacy/pixi.toml",
            "pixi:libs/both/pixi.toml",
            "pixi:pixi.toml",
        ]
    );
}

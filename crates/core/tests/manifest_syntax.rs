//! Manifests are read as the tools that own them read them.
use depsmith_core::{Engine, UpdateOptions};
use std::fs;
use tempfile::tempdir;

/// The failures of `target` in a repository holding `files`, with every
/// tool unavailable.
fn failures(target: &str, files: &[(&str, &str)]) -> Vec<String> {
    let root = tempdir().unwrap();
    for (path, text) in files {
        fs::write(root.path().join(path), text).unwrap();
    }
    let options = UpdateOptions {
        pixi: "depsmith-nonexistent-executable".into(),
        tools: ["uv", "cargo"]
            .map(|tool| (tool.into(), "depsmith-nonexistent-executable".into()))
            .into(),
        ..Default::default()
    };
    let proposal = Engine::default()
        .prepare(root.path(), &[target.into()], options)
        .unwrap();
    proposal.failures.into_iter().map(|f| f.message).collect()
}

/// TOML 1.1 lets an inline table span lines and end with a comma. pixi, uv
/// and cargo accept it, so depsmith reads such a manifest and gets as far
/// as running the tool.
#[test]
fn toml_1_1_manifests_reach_the_tool() {
    let spanning = "items = {\n  one = 1,\n  two = 2,\n}\n";
    for (target, manifest, text, lock) in [
        (
            "pixi:pixi.toml",
            "pixi.toml",
            format!("[workspace]\nname='a'\nchannels=['conda-forge']\nplatforms=['linux-64']\n[package.build.config]\n{spanning}"),
            None,
        ),
        (
            "uv:pyproject.toml",
            "pyproject.toml",
            format!("[project]\nname='a'\nversion='1'\ndependencies=[]\n[tool.other]\n{spanning}"),
            Some(("uv.lock", "version = 1\nrevision = 3\n")),
        ),
        (
            "cargo:Cargo.toml",
            "Cargo.toml",
            format!("[package]\nname='a'\nversion='0.1.0'\n[package.metadata.other]\n{spanning}"),
            Some(("Cargo.lock", "")),
        ),
    ] {
        let mut files = vec![(manifest, &*text)];
        files.extend(lock);
        let failures = failures(target, &files);
        assert!(
            !failures.is_empty() && failures.iter().all(|m| m.contains("depsmith-nonexistent-executable")),
            "{target}: {failures:?}"
        );
    }
}

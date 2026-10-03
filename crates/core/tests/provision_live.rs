//! Opt-in check that the pinned uv download for this host installs and runs.
use depsmith_core::{adapter::Adapter, provision, uv::Uv};

#[test]
#[ignore = "downloads uv from GitHub"]
fn pinned_uv_installs_and_reports_its_version() {
    let cache = tempfile::tempdir().unwrap();
    let tool = Uv::default().spec().tools.remove(0);
    let download = provision::host_download(&tool).expect("a download for this host");
    let version = &tool.tested_versions[0];
    let path = provision::install(&tool.name, version, download, cache.path(), 120).unwrap();
    let output = depsmith_core::process::run(
        &path.to_string_lossy(),
        &["--version".into()],
        cache.path(),
        60,
    )
    .unwrap();
    assert!(output.starts_with(&format!("uv {version}")), "{output}");
}

#[test]
#[ignore = "downloads rustup and a Rust toolchain"]
fn pinned_cargo_installs_a_working_toolchain() {
    use depsmith_core::cargo::Cargo;
    let cache = tempfile::tempdir().unwrap();
    let tool = Cargo::default().spec().tools.remove(0);
    let download = provision::host_download(&tool).expect("a download for this host");
    let version = &tool.tested_versions[0];
    let path = provision::install(&tool.name, version, download, cache.path(), 900).unwrap();
    // The toolchain runs from its final directory, with its recorded env.
    let env = provision::runtime_env(download, &path);
    let output = depsmith_core::process::run_env(
        &path.to_string_lossy(),
        &["--version".into()],
        cache.path(),
        120,
        &env,
    )
    .unwrap();
    assert!(output.starts_with(&format!("cargo {version}")), "{output}");
}

#[test]
#[ignore = "installs conda-lock from PyPI with uv (UV must name a uv executable)"]
fn pinned_conda_lock_installs_hash_locked_with_uv() {
    let uv = std::env::var("UV").expect("UV names a uv executable");
    let cache = tempfile::tempdir().unwrap();
    let tool = depsmith_core::conda::Conda
        .spec()
        .tools
        .into_iter()
        .find(|t| t.name == "conda-lock")
        .unwrap();
    let download = provision::host_download(&tool).expect("a download for this host");
    let version = &tool.tested_versions[0];
    let tools = std::collections::BTreeMap::from([("uv".to_owned(), uv)]);
    let path =
        provision::install_with(&tool.name, version, download, cache.path(), 900, &tools).unwrap();
    // The relocatable venv runs from its final directory.
    let output = depsmith_core::process::run(
        &path.to_string_lossy(),
        &["--version".into()],
        cache.path(),
        120,
    )
    .unwrap();
    assert!(output.contains(version), "{output}");
}

#[test]
#[ignore = "downloads Node.js"]
fn pinned_node_runs_its_bundled_npm() {
    let cache = tempfile::tempdir().unwrap();
    // The npm adapter's tested version must be the npm the Node pin bundles.
    let tool = depsmith_core::Engine::default()
        .specs()
        .into_iter()
        .flat_map(|s| s.tools)
        .find(|t| t.name == "npm")
        .unwrap();
    let tested = tool.tested_versions[0].clone();
    let download = provision::host_download(&tool).expect("a download for this host");
    let path = provision::install("npm", &tested, download, cache.path(), 600).unwrap();
    // npm finds its Node through the recorded PATH.
    let env = provision::runtime_env(download, &path);
    let output = depsmith_core::process::run_env(
        &path.to_string_lossy(),
        &["--version".into()],
        cache.path(),
        120,
        &env,
    )
    .unwrap();
    assert_eq!(
        output.trim(),
        tested,
        "npm bundled with Node {}",
        provision::NODE_VERSION
    );
}

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

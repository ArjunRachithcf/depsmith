//! Which pixi manifests are targets, and how a relock is compared.
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

/// Schema 7 locks packages under a platform name, such as one Pixi derives
/// from `[system-requirements]`. A relock from schema 6 that names the
/// platform moves only the packages that changed, and explains a removed
/// one from the baseline.
#[cfg(unix)]
#[test]
fn a_relock_that_names_a_platform_compares_packages_by_subdir() {
    use depsmith_core::{Package, UpdateOptions};
    use std::{collections::BTreeMap, os::unix::fs::PermissionsExt};

    let url = |name: &str, version: &str| {
        format!("https://conda.anaconda.org/conda-forge/linux-64/{name}-{version}-h0_0.conda")
    };
    // pillow requires zlib, and before the relock also libold.
    let lock = |version: u8, header: &str, platform: &str, zlib: &str, libold: bool| {
        let (zlib, pillow, old) = (
            url("zlib", zlib),
            url("pillow", "11.0.0"),
            url("libold", "1.0"),
        );
        let (listed, record, depends) = if libold {
            (
                format!("      - conda: {old}\n"),
                format!("- conda: {old}\n"),
                "  - libold\n",
            )
        } else {
            Default::default()
        };
        format!("version: {version}\n{header}environments:\n  default:\n    packages:\n      {platform}:\n      - conda: {zlib}\n      - conda: {pillow}\n{listed}packages:\n- conda: {zlib}\n- conda: {pillow}\n  depends:\n  - zlib\n{depends}{record}")
    };
    let root = tempdir().unwrap();
    fs::write(
        root.path().join("pixi.toml"),
        "[workspace]\nname = 'w'\nchannels = ['conda-forge']\nplatforms = ['linux-64']\n[system-requirements]\nlibc = '2.17'\n[dependencies]\npillow = '*'\n",
    )
    .unwrap();
    fs::write(
        root.path().join("pixi.lock"),
        lock(6, "", "linux-64", "1.3.0", true),
    )
    .unwrap();
    let tools = tempdir().unwrap();
    let named = "platforms:\n- name: linux-64-glibc-2-17\n  subdir: linux-64\n  virtual-packages:\n  - __glibc=2.17\n";
    fs::write(
        tools.path().join("next"),
        lock(7, named, "linux-64-glibc-2-17", "1.3.1", false),
    )
    .unwrap();
    // `pixi update --manifest-path M` writes the next lock beside M.
    let pixi = tools.path().join("pixi");
    fs::write(
        &pixi,
        format!(
            "#!/bin/sh\n[ \"$1\" = update ] && cp '{}' \"$(dirname \"$3\")/pixi.lock\"\nexit 0\n",
            tools.path().join("next").display()
        ),
    )
    .unwrap();
    fs::set_permissions(&pixi, fs::Permissions::from_mode(0o755)).unwrap();
    let options = UpdateOptions {
        tools: BTreeMap::from([("pixi".into(), pixi.to_string_lossy().into())]),
        ..Default::default()
    };
    let proposal = Engine::default()
        .prepare(root.path(), &["pixi:pixi.toml".into()], options)
        .unwrap();
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    let side = |p: &Option<Package>| {
        p.as_ref()
            .map(|p| format!("{} {} {}", p.name, p.version, p.platform))
    };
    let moved: Vec<_> = proposal
        .dependencies
        .iter()
        .map(|c| (side(&c.before), side(&c.after), c.introducers.clone()))
        .collect();
    assert_eq!(
        moved,
        [
            (
                Some("zlib 1.3.0 linux-64-glibc-2-17".into()),
                Some("zlib 1.3.1 linux-64-glibc-2-17".into()),
                vec!["pillow".to_owned()]
            ),
            (
                Some("libold 1.0 linux-64-glibc-2-17".into()),
                None,
                vec!["pillow".to_owned()]
            ),
        ]
    );
}

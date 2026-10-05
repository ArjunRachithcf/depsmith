//! Why packages moved: each dependency change of a package the target does
//! not declare names the declared dependencies whose paths reach it.
#![cfg(unix)]
use depsmith_core::{pixi::Pixi, DependencyChange, Engine, UpdateOptions};
use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt};

const MANIFEST: &str = "[workspace]\nname = 'w'\nchannels = ['conda-forge']\nplatforms = ['linux-64']\n\n[dependencies]\nscipy = '*'\nnumpy = '*'\n";

/// A pixi.lock for linux-64 whose packages are `(name-version, depends)`.
fn lock(packages: &[(&str, &[&str])]) -> String {
    let url = |id: &str| format!("https://conda.anaconda.org/conda-forge/linux-64/{id}-h0_0.conda");
    let mut text =
        "version: 6\nenvironments:\n  default:\n    packages:\n      linux-64:\n".to_owned();
    for (id, _) in packages {
        text.push_str(&format!("      - conda: {}\n", url(id)));
    }
    text.push_str("packages:\n");
    for (id, depends) in packages {
        text.push_str(&format!("- conda: {}\n  depends:\n", url(id)));
        for depend in *depends {
            text.push_str(&format!("  - {depend}\n"));
        }
        if depends.is_empty() {
            text.push_str("  []\n");
        }
    }
    text.replace("  depends:\n  []\n", "  depends: []\n")
}

/// Prepare the Pixi target with a stand-in `pixi` that writes `candidate`,
/// from `baseline` when there is one.
fn changes(baseline: Option<&str>, candidate: &str) -> Vec<DependencyChange> {
    let root = tempfile::tempdir().unwrap();
    let tools = tempfile::tempdir().unwrap();
    fs::write(root.path().join("pixi.toml"), MANIFEST).unwrap();
    if let Some(baseline) = baseline {
        fs::write(root.path().join("pixi.lock"), baseline).unwrap();
    }
    let next = tools.path().join("next.lock");
    fs::write(&next, candidate).unwrap();
    let script = tools.path().join("pixi");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\ncase \"$1\" in\n  update) cp '{}' pixi.lock;;\n  lock) :;;\n  *) exit 2;;\nesac\n",
            next.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let options = UpdateOptions {
        tools: BTreeMap::from([("pixi".into(), script.to_string_lossy().into_owned())]),
        ..Default::default()
    };
    let proposal = Engine::new(vec![Box::new(Pixi)])
        .prepare(root.path(), &["pixi:pixi.toml".into()], options)
        .unwrap();
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    proposal.dependencies
}

fn change<'a>(changes: &'a [DependencyChange], name: &str) -> &'a DependencyChange {
    changes
        .iter()
        .find(|c| c.after.as_ref().or(c.before.as_ref()).unwrap().name == name)
        .unwrap_or_else(|| panic!("no change for {name}: {changes:?}"))
}

#[test]
fn changed_added_and_removed_packages_name_their_introducers() {
    let baseline = lock(&[
        ("scipy-1.0", &["numpy >=1", "libblas", "libold"]),
        ("numpy-1.0", &["libblas >=1"]),
        ("libblas-1.0", &[]),
        ("libold-1.0", &[]),
    ]);
    let candidate = lock(&[
        ("scipy-1.0", &["numpy >=1", "libblas"]),
        ("numpy-1.1", &["libblas >=2"]),
        ("libblas-2.0", &["libgfortran"]),
        ("libgfortran-14.0", &[]),
    ]);
    let changes = changes(Some(&baseline), &candidate);

    let libblas = change(&changes, "libblas");
    // Introducers follow the order of the declarations.
    assert_eq!(libblas.introducers, ["numpy", "scipy"]);
    assert_eq!(
        libblas.paths,
        [vec!["numpy", "libblas"], vec!["scipy", "libblas"]]
    );
    let added = change(&changes, "libgfortran");
    assert_eq!(added.introducers, ["numpy", "scipy"]);
    assert_eq!(added.paths[0], ["numpy", "libblas", "libgfortran"]);
    // A removed package is explained by the baseline.
    let removed = change(&changes, "libold");
    assert!(removed.after.is_none());
    assert_eq!(removed.introducers, ["scipy"]);
    // A declared package needs no explanation.
    let numpy = change(&changes, "numpy");
    assert!(numpy.introducers.is_empty() && numpy.paths.is_empty());
}

#[test]
fn without_a_baseline_lock_every_added_package_is_explained() {
    let candidate = lock(&[
        ("scipy-1.0", &["libblas"]),
        ("numpy-1.0", &[]),
        ("libblas-1.0", &[]),
    ]);
    let changes = changes(None, &candidate);
    assert_eq!(change(&changes, "libblas").introducers, ["scipy"]);
}

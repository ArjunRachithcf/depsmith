//! The Pixi adapter with a stand-in `pixi`: what it is asked to update.
#![cfg(unix)]
use depsmith_core::{pixi::Pixi, Engine, UpdateOptions};
use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt};

const MANIFEST: &str = "[workspace]\nname = 'w'\nchannels = ['conda-forge']\nplatforms = ['linux-64']\n\n[pypi-dependencies]\nGitPython = '>=3.1'\n";

const LOCK: &str = "version: 6\nenvironments:\n  default:\n    channels:\n    - url: https://conda.anaconda.org/conda-forge/\n    packages:\n      linux-64:\n      - pypi: https://files.pythonhosted.org/packages/gitpython-3.1.0-py3-none-any.whl\npackages:\n- pypi: https://files.pythonhosted.org/packages/gitpython-3.1.0-py3-none-any.whl\n  name: gitpython\n  version: 3.1.0\n  sha256: abc\n";

#[test]
fn a_pypi_package_is_updated_by_the_name_its_lock_records() {
    let root = tempfile::tempdir().unwrap();
    let tools = tempfile::tempdir().unwrap();
    fs::write(root.path().join("pixi.toml"), MANIFEST).unwrap();
    fs::write(root.path().join("pixi.lock"), LOCK).unwrap();
    let args = tools.path().join("args");
    let script = tools.path().join("pixi");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\ncase \"$1\" in\n  update) echo \"$*\" > '{}';;\n  lock) :;;\n  *) exit 2;;\nesac\n",
            args.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let options = UpdateOptions {
        tools: BTreeMap::from([("pixi".into(), script.to_string_lossy().into_owned())]),
        packages: vec!["GitPython".into()],
        ..Default::default()
    };
    let proposal = Engine::new(vec![Box::new(Pixi)])
        .prepare(root.path(), &["pixi:pixi.toml".into()], options)
        .unwrap();
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    let args = fs::read_to_string(args).unwrap();
    assert!(args.trim_end().ends_with(" gitpython"), "{args}");
}

#[test]
fn a_whole_update_names_every_package_but_git_pins() {
    let git = "git+https://github.com/o/dep.git?rev=abc#0123456789abcdef";
    let lock = format!(
        "version: 6\nenvironments:\n  default:\n    channels:\n    - url: https://conda.anaconda.org/conda-forge/\n    packages:\n      linux-64:\n      - conda: https://conda.anaconda.org/conda-forge/linux-64/zlib-1.3.1-h0_0.conda\n      - pypi: {git}\npackages:\n- conda: https://conda.anaconda.org/conda-forge/linux-64/zlib-1.3.1-h0_0.conda\n- pypi: {git}\n  name: dep\n  version: 0.1.0\n"
    );
    let root = tempfile::tempdir().unwrap();
    let tools = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("pixi.toml"),
        format!("{MANIFEST}dep = {{ git = 'https://github.com/o/dep.git', rev = 'abc' }}\n"),
    )
    .unwrap();
    fs::write(root.path().join("pixi.lock"), lock).unwrap();
    let args = tools.path().join("args");
    let script = tools.path().join("pixi");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\ncase \"$1\" in\n  update) echo \"$*\" > '{}';;\n  lock) :;;\n  *) exit 2;;\nesac\n",
            args.display()
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
    let args = fs::read_to_string(args).unwrap();
    let words: Vec<&str> = args.split_whitespace().collect();
    assert!(words.contains(&"zlib"), "{args}");
    assert!(!words.contains(&"dep"), "{args}");
}

//! Opt-in acceptance test with the real conda-lock and a conda solver against
//! conda-forge (sharded repodata) and pypi.org: pins are suggested with
//! evidence, accepted, applied and rechecked. `CONDA_LOCK` and
//! `CONDA_SOLVER` name the executables (default `conda-lock`, `micromamba`).
use depsmith_core::{apply, Engine, UpdateOptions};
use std::{collections::BTreeMap, fs};

fn options(accept: &[&str]) -> UpdateOptions {
    let tool = |variable: &str, default: &str| std::env::var(variable).unwrap_or(default.into());
    UpdateOptions {
        tools: BTreeMap::from([
            ("conda-lock".into(), tool("CONDA_LOCK", "conda-lock")),
            ("conda".into(), tool("CONDA_SOLVER", "micromamba")),
        ]),
        accept: accept.iter().map(|a| a.to_string()).collect(),
        timeout_seconds: 1800,
        ..Default::default()
    }
}

#[test]
#[ignore = "runs conda-lock against conda-forge and pypi.org"]
fn live_environment_suggests_accepts_applies_and_rechecks() {
    let root = tempfile::tempdir().unwrap();
    let environment = "name: live\nchannels:\n  - conda-forge\ndependencies:\n  - python=3.12\n  \
                       - six ==1.16.0  # pinned\n  - pip\n  - pip:\n    - idna==3.6\nplatforms:\n  - linux-64\n";
    fs::write(root.path().join("environment.yml"), environment).unwrap();
    let engine = Engine::default();
    let target = ["conda:environment.yml".into()];

    let preview = engine.prepare(root.path(), &target, options(&[])).unwrap();
    assert!(preview.failures.is_empty(), "{:?}", preview.failures);
    let six = preview
        .suggestions
        .iter()
        .find(|s| s.package == "six")
        .unwrap_or_else(|| panic!("no six suggestion: {:?}", preview.suggestions));
    assert!(
        six.evidence[0].starts_with("linux-64: 1.")
            && six.evidence[0].contains("https://conda.anaconda.org/conda-forge/noarch/six-"),
        "{:?}",
        six.evidence
    );
    assert!(
        preview.suggestions.iter().any(|s| s.package == "idna"),
        "{:?}",
        preview.suggestions
    );
    assert!(preview.dependencies.iter().any(|d| d
        .after
        .as_ref()
        .is_some_and(|p| p.name == "idna" && p.ecosystem == "pypi")));

    let accepted = engine
        .prepare(root.path(), &target, options(&["six", "idna"]))
        .unwrap();
    assert!(accepted.failures.is_empty(), "{:?}", accepted.failures);
    let after = &accepted
        .changes
        .iter()
        .find(|c| c.path.as_os_str() == "environment.yml")
        .unwrap()
        .after;
    assert!(
        after.contains("  - six ==1.") && after.contains("  # pinned\n"),
        "{after}"
    );
    assert!(
        !after.contains("==1.16.0") && !after.contains("idna==3.6\n"),
        "{after}"
    );
    apply(&accepted, false).unwrap();

    let recheck = engine.prepare(root.path(), &target, options(&[])).unwrap();
    assert!(recheck.failures.is_empty(), "{:?}", recheck.failures);
    assert!(
        !recheck
            .suggestions
            .iter()
            .any(|s| s.package == "six" || s.package == "idna"),
        "{:?}",
        recheck.suggestions
    );
}

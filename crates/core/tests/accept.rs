use depsmith_core::{Engine, Error, Proposal, UpdateOptions};
use std::fs;
use tempfile::tempdir;

const PIXI: &str = "[workspace]\nname='a'\nchannels=['conda-forge']\nplatforms=['linux-64']\n";

fn options(accept: &[&str]) -> UpdateOptions {
    UpdateOptions {
        pixi: "depsmith-nonexistent-executable".into(),
        accept: accept.iter().map(|a| a.to_string()).collect(),
        ..Default::default()
    }
}

fn prepare(manifest: &str, accept: &[&str]) -> depsmith_core::Result<Proposal> {
    let root = tempdir().unwrap();
    fs::write(root.path().join("pixi.toml"), format!("{PIXI}{manifest}")).unwrap();
    Engine::default().prepare(root.path(), &["pixi:pixi.toml".into()], options(accept))
}

#[test]
fn malformed_or_undeclared_acceptances_are_rejected() {
    let manifest = "[dependencies]\nruff='==0.15.22'\n";
    for (accept, needle) in [(">=1", "NAME=REQUIREMENT"), ("ruf", "direct dependency")] {
        let error = prepare(manifest, &[accept]).unwrap_err();
        assert!(
            matches!(&error, Error::Invalid(m) if m.contains(needle)),
            "{accept}: {error}"
        );
    }
}

#[test]
fn acceptance_requires_adapter_support() {
    let root = tempdir().unwrap();
    let workflows = root.path().join(".github/workflows");
    fs::create_dir_all(&workflows).unwrap();
    fs::write(
        workflows.join("ci.yml"),
        "on: push\njobs:\n  b:\n    runs-on: x\n    steps:\n      - uses: actions/checkout@v4\n",
    )
    .unwrap();
    let error = Engine::default()
        .prepare(
            root.path(),
            &["github-actions:.github/workflows/ci.yml".into()],
            options(&["actions/checkout"]),
        )
        .unwrap_err();
    assert!(
        matches!(&error, Error::Invalid(m) if m.contains("--accept") && m.contains("github-actions")),
        "{error}"
    );
}

/// Code 2 with our message (not the missing backend's code 3) proves the
/// checks ran before any backend or network access.
fn rejected_before_backend(manifest: &str, accept: &str, needle: &str) {
    let proposal = prepare(manifest, &[accept]).unwrap();
    assert_eq!(proposal.failures.len(), 1, "{:?}", proposal.failures);
    let failure = &proposal.failures[0];
    assert_eq!(failure.code, 2, "{}", failure.message);
    assert!(failure.message.contains(needle), "{}", failure.message);
}

#[test]
fn ambiguous_requirement_needs_explicit_replacement() {
    rejected_before_backend(
        "[dependencies]\nfoo='>=1,<2'\n",
        "foo",
        "--accept foo=REQUIREMENT",
    );
}

#[test]
fn explicit_pypi_requirement_must_be_pep_440() {
    rejected_before_backend(
        "[pypi-dependencies]\nsix='==1.15.0'\n",
        "six==1.17.0",
        "PEP 440",
    );
}

#[test]
fn unversioned_declaration_cannot_be_accepted() {
    rejected_before_backend(
        "[dependencies]\nfoo='*'\n",
        "foo",
        "--accept foo=REQUIREMENT",
    );
}

#[test]
fn explicit_replacement_proceeds_to_the_backend_without_lookups() {
    let proposal = prepare("[dependencies]\nruff='==0.15.22'\n", &["ruff===0.16.9"]).unwrap();
    assert_eq!(proposal.failures.len(), 1);
    assert_eq!(
        proposal.failures[0].code, 3,
        "{}",
        proposal.failures[0].message
    );
    assert!(proposal.failures[0].message.contains("cannot start"));
}

fn prepare_pyproject(manifest: &str, accept: &[&str]) -> depsmith_core::Result<Proposal> {
    let root = tempdir().unwrap();
    let pixi = PIXI.replace("[workspace]", "[tool.pixi.workspace]");
    fs::write(
        root.path().join("pyproject.toml"),
        format!("{manifest}{pixi}"),
    )
    .unwrap();
    Engine::default().prepare(
        root.path(),
        &["pixi:pyproject.toml".into()],
        options(accept),
    )
}

fn failure(proposal: &Proposal) -> (u8, &str) {
    assert_eq!(proposal.failures.len(), 1, "{:?}", proposal.failures);
    let failure = &proposal.failures[0];
    (failure.code, &failure.message)
}

#[test]
fn standard_pyproject_requirements_can_be_accepted() {
    let manifest = "[project]\nname='p'\nversion='0'\ndependencies=[\"six[x] ==1.15.0; os_name == 'nt'\", 'foo>=1,<2', 'direct @ https://example.invalid/d.tar.gz']\n";
    // Explicit replacement rewrites the [project] string and reaches the backend.
    let proposal = prepare_pyproject(manifest, &["six===1.17.0"]).unwrap();
    let (code, message) = failure(&proposal);
    assert_eq!(code, 3, "{message}");
    assert!(message.contains("cannot start"), "{message}");
    for (accept, needle) in [
        ("foo", "--accept foo=REQUIREMENT"),
        ("direct", "no version requirement"),
        ("six=1.17", "PEP 440"),
    ] {
        let proposal = prepare_pyproject(manifest, &[accept]).unwrap();
        let (code, message) = failure(&proposal);
        assert_eq!(code, 2, "{accept}: {message}");
        assert!(message.contains(needle), "{accept}: {message}");
    }
}

use depsmith_core::{
    actions::workflow_inventory,
    scan::{inventory_sbom, IdentityMapping},
    Package,
};
fn package(ecosystem: &str, name: &str, version: &str) -> Package {
    Package {
        ecosystem: ecosystem.into(),
        name: name.into(),
        version: version.into(),
        artifact: "https://example.org/linux-64/pkg.conda".into(),
        platform: "linux-64".into(),
    }
}
#[test]
fn conda_names_are_never_assumed_to_be_pypi_identities() {
    let (sbom, unknown) = inventory_sbom(&[package("conda", "requests", "2.0.0")], &[]).unwrap();
    assert_eq!(unknown.len(), 1);
    assert_eq!(sbom["components"].as_array().unwrap().len(), 0);
}
#[test]
fn registry_python_identity_is_included_and_mapping_retains_evidence() {
    let mapping = IdentityMapping {
        ecosystem: "conda".into(),
        name: "python-requests".into(),
        purl: "pkg:pypi/requests".into(),
        evidence: "https://github.com/conda-forge/requests-feedstock/blob/commit/recipe/meta.yaml"
            .into(),
    };
    let mut public = package("pypi", "urllib3", "1.26.5");
    public.artifact = "https://files.pythonhosted.org/packages/urllib3.whl".into();
    let (sbom, unknown) = inventory_sbom(
        &[public, package("conda", "python-requests", "2.0.0")],
        &[mapping],
    )
    .unwrap();
    assert!(unknown.is_empty());
    assert_eq!(sbom["components"][0]["purl"], "pkg:pypi/urllib3@1.26.5");
    assert_eq!(sbom["components"][1]["purl"], "pkg:pypi/requests@2.0.0");
    assert!(sbom["components"][1]["properties"]
        .to_string()
        .contains("evidence"));
}
#[test]
fn private_python_artifacts_require_explicit_identity_evidence() {
    let private = package("pypi", "requests", "2.0.0");
    let (sbom, unknown) = inventory_sbom(&[private], &[]).unwrap();
    assert_eq!(unknown.len(), 1);
    assert_eq!(sbom["components"].as_array().unwrap().len(), 0);
}
#[test]
fn github_action_inventory_keeps_the_repository_identity() {
    let action = Package {
        ecosystem: "github-actions".into(),
        name: "actions/checkout".into(),
        version: "v4.2.1".into(),
        artifact: "https://github.com/actions/checkout@v4.2.1".into(),
        platform: "workflow".into(),
    };
    let (sbom, unknown) = inventory_sbom(&[action], &[]).unwrap();
    assert!(unknown.is_empty());
    assert_eq!(
        sbom["components"][0]["purl"],
        "pkg:github/actions/checkout@v4.2.1"
    );
}

#[test]
fn unknown_placeholder_is_not_an_assessed_version() {
    let mut record = package("pypi", "demo", "unknown");
    record.artifact = "https://files.pythonhosted.org/packages/demo.whl".into();
    let (sbom, unknown) = inventory_sbom(&[record], &[]).unwrap();
    assert_eq!(unknown.len(), 1);
    assert!(sbom["components"].as_array().unwrap().is_empty());
}

#[test]
fn only_exact_action_tags_are_assessed() {
    let sha = "b4ffde65f46336ab88eb53be808477a3936bae11";
    let workflow = format!(
        "jobs:\n  b:\n    steps:\n      - uses: actions/checkout@v4.2.1\n      - uses: actions/setup-python@{sha} # v5.1.0\n      - uses: actions/cache@v4\n      - uses: org/tool@main\n"
    );
    let (sbom, unknown) = inventory_sbom(&workflow_inventory(&workflow).unwrap(), &[]).unwrap();
    let assessed: Vec<_> = sbom["components"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["purl"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(assessed, ["pkg:github/actions/checkout@v4.2.1"]);
    // A SHA pin's version comment is not evidence; floating refs are unassessed.
    let unassessed: Vec<_> = unknown.iter().map(|p| p.version.as_str()).collect();
    assert_eq!(unassessed, [sha, "v4", "main"]);
}

#[test]
fn same_name_in_conda_and_pypi_stays_two_identities() {
    let mapping = IdentityMapping {
        ecosystem: "conda".into(),
        name: "urllib3".into(),
        purl: "pkg:pypi/urllib3".into(),
        evidence: "https://github.com/conda-forge/urllib3-feedstock".into(),
    };
    let mut pypi = package("pypi", "urllib3", "1.26.5");
    pypi.artifact = "https://files.pythonhosted.org/packages/urllib3.whl".into();
    let conda = package("conda", "urllib3", "1.26.5");
    let (sbom, unknown) = inventory_sbom(&[conda, pypi], &[mapping]).unwrap();
    assert!(unknown.is_empty());
    let components = sbom["components"].as_array().unwrap();
    assert_eq!(components.len(), 2);
    // Same PURL, but each keeps its own ecosystem and applicability.
    assert!(components
        .iter()
        .all(|c| c["purl"] == "pkg:pypi/urllib3@1.26.5"));
    let text = sbom.to_string();
    assert!(text.contains("\\\"ecosystem\\\":\\\"conda\\\""), "{text}");
    assert!(
        text.contains("unknown: upstream advisory does not establish conda build/backport status")
    );
}

#[test]
fn duplicate_or_unproven_mappings_are_rejected() {
    let mapping = |purl: &str, evidence: &str| IdentityMapping {
        ecosystem: "conda".into(),
        name: "openssl".into(),
        purl: purl.into(),
        evidence: evidence.into(),
    };
    let good = mapping("pkg:generic/openssl", "https://example.org/feedstock");
    for mappings in [
        vec![good.clone(), good.clone()],
        vec![mapping("pkg:generic/openssl", "http://insecure.example")],
        vec![mapping(
            "pkg:generic/openssl@3.0.0",
            "https://example.org/feedstock",
        )],
    ] {
        assert!(inventory_sbom(&[], &mappings).is_err(), "{mappings:?}");
    }
}

#[test]
fn scanning_a_pixi_target_without_a_lock_names_the_missing_file() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("pixi.toml"),
        "[workspace]\nname = 'demo'\n",
    )
    .unwrap();
    let options = depsmith_core::UpdateOptions {
        scan: true,
        ..Default::default()
    };
    let error = depsmith_core::scan_existing(root.path(), &["pixi:pixi.toml".into()], &options)
        .unwrap_err();
    assert!(
        matches!(&error, depsmith_core::Error::Invalid(m) if m.contains("pixi.lock") && m.contains("pixi lock")),
        "{error}"
    );
}

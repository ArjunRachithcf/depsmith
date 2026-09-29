//! Offline scan pipeline tests against a scripted stand-in for Grype.
#![cfg(unix)]
use depsmith_core::{
    scan::{scan_pair, IdentityMapping, Suppression},
    Error, Package, UpdateOptions,
};
use serde_json::json;
use std::{fs, os::unix::fs::PermissionsExt, path::Path};
use tempfile::TempDir;

fn urllib3(version: &str) -> Package {
    Package {
        ecosystem: "pypi".into(),
        name: "urllib3".into(),
        version: version.into(),
        artifact: format!("https://files.pythonhosted.org/packages/urllib3-{version}.whl"),
        platform: "linux-64".into(),
    }
}

fn conda_openssl() -> Package {
    Package {
        ecosystem: "conda".into(),
        name: "openssl".into(),
        version: "3.0.0".into(),
        artifact: "https://conda.anaconda.org/conda-forge/linux-64/openssl-3.0.0-h7f98852_0.conda"
            .into(),
        platform: "linux-64".into(),
    }
}

fn finding(purl: &str, id: &str, severity: &str) -> serde_json::Value {
    json!({"artifact": {"purl": purl},
           "vulnerability": {"id": id, "namespace": "github:language:python", "severity": severity},
           "matchDetails": [{"type": "exact-direct-match"}]})
}

/// A `grype` stand-in: `db update`/`db status` succeed; scanning an SBOM whose
/// file name contains "before"/"after" prints the matching fixture.
fn fake_grype(
    before: serde_json::Value,
    after: serde_json::Value,
    scan_exit: i32,
) -> (TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, value: &serde_json::Value| {
        fs::write(dir.path().join(name), value.to_string()).unwrap();
    };
    write("before.json", &json!({"matches": before}));
    write("after.json", &json!({"matches": after}));
    let script = dir.path().join("grype");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nD='{}'\ncase \"$1\" in\n  db) [ \"$2\" = status ] && echo '{{\"built\":\"2026-09-28T00:00:00Z\",\"schemaVersion\":\"v6\"}}'; exit 0;;\n  sbom:*before*) cat \"$D/before.json\"; exit {scan_exit};;\n  sbom:*) cat \"$D/after.json\"; exit {scan_exit};;\nesac\nexit 2\n",
            dir.path().display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let path = script.to_string_lossy().into_owned();
    (dir, path)
}

fn options(grype: &str) -> UpdateOptions {
    UpdateOptions {
        scan: true,
        grype: grype.into(),
        ..Default::default()
    }
}

fn cwd() -> &'static Path {
    Path::new(".")
}

#[test]
fn missing_baseline_is_reported_as_candidate_only() {
    let (_dir, grype) = fake_grype(
        json!([]),
        json!([finding("pkg:pypi/urllib3@1.26.5", "GHSA-a", "High")]),
        0,
    );
    let mut options = options(&grype);
    options.fail_on = Some("high".into());
    options.only_new = true;
    let report = scan_pair("t", None, &[urllib3("1.26.5")], &options, cwd()).unwrap();
    assert_eq!(report.comparison, "candidate-only");
    assert!(!report.baseline_available);
    assert_eq!(report.findings.len(), 1);
    assert!(
        report.introduced.is_empty() && report.resolved.is_empty() && report.remaining.is_empty()
    );
    // "New" cannot be established without a baseline, so every finding is assessed.
    assert!(!report.policy_passed);
}

#[test]
fn baseline_comparison_classifies_findings() {
    let (_dir, grype) = fake_grype(
        json!([
            finding("pkg:pypi/urllib3@1.26.5", "GHSA-fixed", "High"),
            finding("pkg:pypi/urllib3@1.26.5", "GHSA-stays", "Low")
        ]),
        json!([
            finding("pkg:pypi/urllib3@1.26.17", "GHSA-stays", "Low"),
            finding("pkg:pypi/urllib3@1.26.17", "GHSA-new", "Medium")
        ]),
        0,
    );
    let report = scan_pair(
        "t",
        Some(&[urllib3("1.26.5")]),
        &[urllib3("1.26.17")],
        &options(&grype),
        cwd(),
    )
    .unwrap();
    assert_eq!(report.comparison, "baseline");
    let ids = |list: &[depsmith_core::scan::Finding]| {
        list.iter().map(|f| f.id.clone()).collect::<Vec<_>>()
    };
    assert_eq!(ids(&report.resolved), ["GHSA-fixed"]);
    assert_eq!(ids(&report.remaining), ["GHSA-stays"]);
    assert_eq!(ids(&report.introduced), ["GHSA-new"]);
    assert_eq!(ids(&report.findings), ["GHSA-new", "GHSA-stays"]);
}

#[test]
fn scanner_failures_and_untraceable_output_are_errors() {
    let (_dir, failing) = fake_grype(json!([]), json!([]), 1);
    let error = scan_pair("t", None, &[urllib3("1.0.0")], &options(&failing), cwd()).unwrap_err();
    assert!(matches!(error, Error::Operation(_)), "{error}");
    let (_dir, stray) = fake_grype(
        json!([]),
        json!([finding("pkg:pypi/other@1.0.0", "GHSA-x", "Low")]),
        0,
    );
    let error = scan_pair("t", None, &[urllib3("1.0.0")], &options(&stray), cwd()).unwrap_err();
    assert!(error.to_string().contains("cannot be traced"), "{error}");
}

#[test]
fn findings_keep_conda_provenance_and_backport_uncertainty() {
    let mapping = IdentityMapping {
        ecosystem: "conda".into(),
        name: "openssl".into(),
        purl: "pkg:generic/openssl".into(),
        evidence: "https://github.com/conda-forge/openssl-feedstock".into(),
    };
    let (_dir, grype) = fake_grype(
        json!([]),
        json!([
            finding("pkg:generic/openssl@3.0.0", "CVE-conda", "High"),
            finding("pkg:pypi/urllib3@1.26.5", "GHSA-a", "High")
        ]),
        0,
    );
    let mut options = options(&grype);
    options.identity_mappings = vec![mapping];
    let report = scan_pair(
        "t",
        None,
        &[conda_openssl(), urllib3("1.26.5")],
        &options,
        cwd(),
    )
    .unwrap();
    let conda = report
        .findings
        .iter()
        .find(|f| f.id == "CVE-conda")
        .unwrap();
    assert!(
        conda.applicability.starts_with("unknown"),
        "{}",
        conda.applicability
    );
    assert_eq!(conda.artifact, conda_openssl().artifact);
    let upstream = report.findings.iter().find(|f| f.id == "GHSA-a").unwrap();
    assert_eq!(upstream.applicability, "upstream");
}

fn suppression(package: Option<&str>, target: Option<&str>, expires: Option<&str>) -> Suppression {
    Suppression {
        id: "GHSA-a".into(),
        package: package.map(Into::into),
        target: target.map(Into::into),
        reason: "not reachable: we never parse untrusted URLs".into(),
        expires: expires.map(Into::into),
    }
}

#[test]
fn scoped_suppressions_are_reported_and_excluded_from_gates() {
    let (_dir, grype) = fake_grype(
        json!([]),
        json!([finding("pkg:pypi/urllib3@1.26.5", "GHSA-a", "High")]),
        0,
    );
    let scan = |suppressions: Vec<Suppression>| {
        let mut options = options(&grype);
        options.fail_on = Some("high".into());
        options.suppressions = suppressions;
        options.validate().unwrap();
        scan_pair(
            "pixi:pixi.toml",
            None,
            &[urllib3("1.26.5")],
            &options,
            cwd(),
        )
        .unwrap()
    };
    let report = scan(vec![suppression(
        Some("pypi:urllib3"),
        None,
        Some("2999-01-01"),
    )]);
    assert!(report.policy_passed);
    let applied = report.findings[0]
        .suppression
        .as_ref()
        .expect("suppression retained");
    assert!(applied.reason.contains("never parse"));
    // Scope must match: another package or target leaves the finding active.
    for other in [
        suppression(Some("pypi:requests"), None, None),
        suppression(None, Some("pixi:other/pixi.toml"), None),
    ] {
        let report = scan(vec![other]);
        assert!(!report.policy_passed);
        assert!(report.findings[0].suppression.is_none());
    }
    let expired = scan(vec![suppression(
        Some("pypi:urllib3"),
        None,
        Some("2020-01-01"),
    )]);
    assert!(!expired.policy_passed);
    assert_eq!(expired.expired_suppressions.len(), 1);
}

#[test]
fn unscoped_or_undocumented_suppressions_are_rejected() {
    let invalid = [
        suppression(None, None, None),
        Suppression {
            reason: " ".into(),
            ..suppression(Some("pypi:urllib3"), None, None)
        },
        Suppression {
            id: "".into(),
            ..suppression(Some("pypi:urllib3"), None, None)
        },
        suppression(Some("urllib3"), None, None),
        suppression(Some("pypi:urllib3"), None, Some("next week")),
    ];
    for suppression in invalid {
        let options = UpdateOptions {
            suppressions: vec![suppression.clone()],
            ..Default::default()
        };
        assert!(
            matches!(options.validate(), Err(Error::Invalid(_))),
            "{suppression:?}"
        );
    }
}

struct Fixture;
impl depsmith_core::adapter::Adapter for Fixture {
    fn spec(&self) -> depsmith_core::adapter::AdapterSpec {
        depsmith_core::adapter::AdapterSpec::new("fixture", &["project.toml"])
    }
    fn detects(&self, path: &Path, _: &str) -> bool {
        path == Path::new("project.toml")
    }
    fn prepare(
        &self,
        root: &Path,
        _: &depsmith_core::Target,
        _: &UpdateOptions,
    ) -> depsmith_core::Result<depsmith_core::adapter::Candidate> {
        fs::write(root.join("project.lock"), "candidate\n")?;
        Ok(depsmith_core::adapter::Candidate {
            files: vec!["project.lock".into()],
            before: vec![urllib3("1.26.5")],
            baseline_available: true,
            after: vec![urllib3("1.26.17")],
            ..Default::default()
        })
    }
}

#[test]
fn scanner_failure_blocks_the_affected_proposal() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("project.toml"), "").unwrap();
    fs::write(root.path().join("project.lock"), "old\n").unwrap();
    let (_dir, failing) = fake_grype(json!([]), json!([]), 1);
    let proposal = depsmith_core::Engine::new(vec![Box::new(Fixture)])
        .prepare(
            root.path(),
            &["fixture:project.toml".into()],
            options(&failing),
        )
        .unwrap();
    assert_eq!(proposal.failures.len(), 1);
    assert_eq!(proposal.failures[0].code, 3);
    assert!(
        proposal.changes.is_empty(),
        "a failed scan must not offer changes"
    );
    assert_eq!(proposal.exit_code(true), 3);
}

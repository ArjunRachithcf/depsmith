//! Explicit network acceptance test. Requires GRYPE or grype on PATH.
use depsmith_core::{scan::scan_pair, Package, UpdateOptions};

#[test]
#[ignore = "downloads a public vulnerability database and runs native Grype"]
fn real_grype_compares_vulnerable_and_patched_versions_with_one_snapshot() {
    let vulnerable = Package {
        ecosystem: "pypi".into(),
        name: "urllib3".into(),
        version: "1.26.5".into(),
        artifact: "https://files.pythonhosted.org/packages/urllib3.whl".into(),
        platform: "linux-64".into(),
    };
    // https://github.com/advisories/GHSA-v845-jxx5-vc9f: patched in 1.26.17.
    // This version can still have other advisories; do not assert it is clean.
    let patched = Package {
        version: "1.26.17".into(),
        ..vulnerable.clone()
    };
    let misleading = Package {
        ecosystem: "conda".into(),
        artifact: "https://example.org/urllib3.conda".into(),
        ..vulnerable.clone()
    };
    let options = UpdateOptions {
        scan: true,
        grype: std::env::var("GRYPE").unwrap_or("grype".into()),
        timeout_seconds: 600,
        ..Default::default()
    };
    let root = tempfile::tempdir().unwrap();
    let report = scan_pair(
        "fixture",
        Some(&[vulnerable, misleading.clone()]),
        &[patched, misleading],
        &options,
        root.path(),
    )
    .unwrap();
    assert!(report.baseline_available);
    assert!(report
        .resolved
        .iter()
        .any(|f| f.id == "GHSA-v845-jxx5-vc9f"));
    assert!(!report
        .remaining
        .iter()
        .chain(&report.introduced)
        .any(|f| f.id == "GHSA-v845-jxx5-vc9f"));
    assert_eq!(report.unknown_before.len(), 1);
    assert_eq!(report.unknown_after.len(), 1);
    assert!(report.database["valid"].as_bool().unwrap());
    assert_eq!(report.comparison, "baseline");
    let fixed = report
        .resolved
        .iter()
        .find(|f| f.id == "GHSA-v845-jxx5-vc9f")
        .unwrap();
    assert_eq!(fixed.applicability, "upstream");
    assert_eq!(
        fixed.artifact,
        "https://files.pythonhosted.org/packages/urllib3.whl"
    );
}

#[test]
#[ignore = "downloads a public vulnerability database and runs native Grype"]
fn real_grype_candidate_only_scan_with_scoped_suppression() {
    let vulnerable = Package {
        ecosystem: "pypi".into(),
        name: "urllib3".into(),
        version: "1.26.5".into(),
        artifact: "https://files.pythonhosted.org/packages/urllib3.whl".into(),
        platform: "linux-64".into(),
    };
    let options = UpdateOptions {
        scan: true,
        grype: std::env::var("GRYPE").unwrap_or("grype".into()),
        timeout_seconds: 600,
        fail_on: Some("critical".into()),
        suppressions: vec![depsmith_core::scan::Suppression {
            id: "GHSA-v845-jxx5-vc9f".into(),
            package: Some("pypi:urllib3".into()),
            target: None,
            reason: "fixture: accepted risk for testing".into(),
            expires: None,
        }],
        ..Default::default()
    };
    options.validate().unwrap();
    let root = tempfile::tempdir().unwrap();
    let report = scan_pair("fixture", None, &[vulnerable], &options, root.path()).unwrap();
    assert_eq!(report.comparison, "candidate-only");
    assert!(report.introduced.is_empty() && report.resolved.is_empty());
    let suppressed = report
        .findings
        .iter()
        .find(|f| f.id == "GHSA-v845-jxx5-vc9f")
        .unwrap();
    assert!(suppressed.suppression.is_some());
    // Other advisories for this version remain active and unsuppressed.
    assert!(report.findings.iter().any(|f| f.suppression.is_none()));
}

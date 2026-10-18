//! Security updates: which findings are actionable, and the fix version each
//! one aims at (ADR 0004).
use depsmith_core::{
    config::settings,
    scan::{actionable, fix_version, Finding, FixState, Suppression},
    SecurityTarget,
};
use serde_json::json;

fn finding(identity: &str, version: &str, severity: &str, fixes: &[&str]) -> Finding {
    let package = identity.split(':').nth(1).unwrap();
    serde_json::from_value(
        json!({"id": format!("GHSA-{package}-{severity}"), "namespace": "github",
        "package": package, "version": version, "severity": severity, "evidence": [],
        "identity": identity, "applicability": "upstream", "artifact": "", "suppression": null,
        "fix_versions": fixes,
        "fix_state": if fixes.is_empty() { "not-fixed" } else { "fixed" }}),
    )
    .unwrap()
}

fn ids(findings: Vec<&Finding>) -> Vec<String> {
    findings.into_iter().map(|f| f.id.clone()).collect()
}

#[test]
fn actionable_findings_are_fixed_unsuppressed_and_severe_enough() {
    let mut suppressed = finding("pypi:jinja2:any", "3.1.2", "High", &["3.1.3"]);
    suppressed.suppression = Some(Suppression {
        id: suppressed.id.clone(),
        package: Some("pypi:jinja2".into()),
        target: None,
        reason: "not reachable".into(),
        expires: None,
    });
    // Fixed releases without a state still fix it; a won't-fix does not.
    let mut unknown_state = finding("pypi:idna:any", "3.6", "High", &["3.7"]);
    unknown_state.fix_state = FixState::Unknown;
    let mut wont_fix = finding("pypi:wheel:any", "0.37", "High", &["0.38"]);
    wont_fix.fix_state = FixState::WontFix;
    let findings = [
        finding("pypi:urllib3:any", "1.26.5", "Low", &["1.26.18"]),
        finding("pypi:requests:any", "2.30.0", "Critical", &["2.31.0"]),
        finding("pypi:certifi:any", "2023.5.7", "High", &[]),
        suppressed,
        unknown_state,
        wont_fix,
        finding("pypi:odd:any", "1.0", "Unrated", &["1.1"]),
    ];
    // Every severity by default.
    assert_eq!(
        ids(actionable(&findings, None).unwrap()),
        [
            "GHSA-urllib3-Low",
            "GHSA-requests-Critical",
            "GHSA-idna-High",
            "GHSA-odd-Unrated"
        ]
    );
    // An unknown severity is never filtered out, as with --fail-on.
    assert_eq!(
        ids(actionable(&findings, Some("high")).unwrap()),
        [
            "GHSA-requests-Critical",
            "GHSA-idna-High",
            "GHSA-odd-Unrated"
        ]
    );
    assert!(actionable(&findings, Some("severe")).is_err());
}

#[test]
fn the_lowest_fix_above_the_locked_version_is_the_default() {
    // Fixes in two release lines: the locked line's fix is the lowest that applies.
    let urllib3 = finding("pypi:urllib3:any", "1.26.5", "High", &["2.0.7", "1.26.18"]);
    assert_eq!(
        fix_version(&urllib3, SecurityTarget::Lowest, &[]).as_deref(),
        Some("1.26.18")
    );
    let newer_line = finding("pypi:urllib3:any", "2.0.2", "High", &["2.0.7", "1.26.18"]);
    assert_eq!(
        fix_version(&newer_line, SecurityTarget::Lowest, &[]).as_deref(),
        Some("2.0.7")
    );
    // Versions compare by each ecosystem's scheme, not as text.
    let serde = finding("cargo:serde:any", "1.0.9", "High", &["1.0.10"]);
    assert_eq!(
        fix_version(&serde, SecurityTarget::Lowest, &[]).as_deref(),
        Some("1.0.10")
    );
    assert_eq!(
        fix_version(
            &finding("pypi:certifi:any", "1.0", "High", &[]),
            SecurityTarget::Lowest,
            &[]
        ),
        None
    );
}

#[test]
fn patch_and_newest_choose_among_the_available_releases() {
    let minimist = finding("npm:minimist:any", "1.2.0", "High", &["1.2.6"]);
    let available: Vec<String> = ["1.2.5", "1.2.6", "1.2.8", "1.3.0", "2.0.1", "1.2.10"]
        .map(String::from)
        .to_vec();
    assert_eq!(
        fix_version(&minimist, SecurityTarget::Patch, &available).as_deref(),
        Some("1.2.10")
    );
    assert_eq!(
        fix_version(&minimist, SecurityTarget::Newest, &available).as_deref(),
        Some("2.0.1")
    );
    // Without known releases, the lowest fix is the best that can be named.
    for target in [SecurityTarget::Patch, SecurityTarget::Newest] {
        assert_eq!(
            fix_version(&minimist, target, &[]).as_deref(),
            Some("1.2.6")
        );
    }
}

#[test]
fn security_options_are_read_from_the_configuration_and_validated() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("depsmith.toml"),
        "[options]\nmin_severity = \"medium\"\nsecurity_target = \"patch\"\n",
    )
    .unwrap();
    let config = settings(root.path(), &json!({})).unwrap();
    assert_eq!(config.options.min_severity.as_deref(), Some("medium"));
    assert_eq!(config.options.security_target, SecurityTarget::Patch);
    let defaults = settings(tempfile::tempdir().unwrap().path(), &json!({})).unwrap();
    assert_eq!(defaults.options.min_severity, None);
    assert_eq!(defaults.options.security_target, SecurityTarget::Lowest);
    assert!(settings(root.path(), &json!({"min_severity": "severe"})).is_err());
    assert!(settings(root.path(), &json!({"security_target": "latest"})).is_err());
}

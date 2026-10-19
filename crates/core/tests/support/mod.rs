//! Helpers shared by integration tests.

/// A Grype stand-in in `dir` that reports `advisory` of `namespace`, fixed
/// in `fix`, whenever a scanned SBOM contains the versioned package URL
/// `purl`, such as `pkg:npm/ms@2.0.0`.
#[cfg(unix)]
pub fn grype(
    dir: &std::path::Path,
    advisory: &str,
    namespace: &str,
    purl: &str,
    fix: &str,
) -> String {
    use std::{fs, os::unix::fs::PermissionsExt};
    let row = serde_json::json!({"artifact": {"purl": purl}, "matchDetails": [],
        "vulnerability": {"id": advisory, "namespace": namespace, "severity": "High",
            "fix": {"versions": [fix], "state": "fixed"}}});
    let path = dir.join("grype");
    fs::write(
        &path,
        format!(
            "#!/bin/sh\ncase \"$1\" in\n  db) [ \"$2\" = status ] && echo '{{\"built\":\"2026-10-01T00:00:00Z\"}}'; exit 0;;\n  sbom:*) if grep -q '\"purl\":\"{purl}\"' \"${{1#sbom:}}\"; then echo '{{\"matches\":[{row}]}}'; else echo '{{\"matches\":[]}}'; fi; exit 0;;\nesac\nexit 2\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path.to_string_lossy().into_owned()
}

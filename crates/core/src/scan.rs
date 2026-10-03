//! Vulnerability scanning: inventories become CycloneDX SBOMs with verified
//! identities, Grype scans the baseline and candidate with one database
//! snapshot, and findings are classified and checked against policy.
use crate::{process::run_env, Error, Package, Result, UpdateOptions};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeSet, fs, path::Path};

/// The vulnerability scanner the engine drives, with the Grype releases
/// exercised by the native acceptance test.
pub fn scanner_tool() -> crate::adapter::ToolSpec {
    crate::adapter::ToolSpec {
        name: "grype".into(),
        default: "grype".into(),
        tested_versions: vec!["0.119.0".into()],

        downloads: crate::provision::pinned("grype"),
    }
}

/// A reviewed statement that a package in one ecosystem is the same software
/// as an upstream identity, used when no verifiable identity can be derived.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityMapping {
    /// Ecosystem of the mapped package, such as `conda`.
    pub ecosystem: String,
    /// Package name in that ecosystem.
    pub name: String,
    /// Upstream identity as an unversioned Package URL, such as `pkg:pypi/urllib3`.
    pub purl: String,
    /// HTTPS link to the provenance that establishes the mapping.
    pub evidence: String,
}
/// A documented, scoped exception for one advisory. Suppressed findings stay
/// in reports and are only excluded from policy gates.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Suppression {
    /// Advisory identifier, e.g. `GHSA-...` or `CVE-...`.
    pub id: String,
    /// `ecosystem:name`, e.g. `pypi:urllib3`.
    #[serde(default)]
    pub package: Option<String>,
    /// Target ID, e.g. `pixi:pixi.toml`.
    #[serde(default)]
    pub target: Option<String>,
    /// Why the finding is acceptable; required.
    pub reason: String,
    /// Last day (`YYYY-MM-DD`, UTC) the suppression applies.
    #[serde(default)]
    pub expires: Option<String>,
}
/// One advisory matched to one inventory package.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    /// Advisory identifier, such as `GHSA-…` or `CVE-…`.
    pub id: String,
    /// Advisory namespace reported by the scanner.
    pub namespace: String,
    /// Name of the affected package.
    pub package: String,
    /// Version of the affected package.
    pub version: String,
    /// Severity as reported, such as `High`.
    pub severity: String,
    /// The scanner's match details, kept verbatim.
    pub evidence: Value,
    /// Package identity `ecosystem:name:platform`, used to compare baseline and
    /// candidate findings and to scope suppressions.
    pub identity: String,
    /// `upstream`, or why the advisory's applicability to this build is unknown.
    pub applicability: String,
    /// Resolved artifact (for conda: channel, subdir and build).
    pub artifact: String,
    /// The suppression that covers this finding, if any.
    pub suppression: Option<Suppression>,
}
/// Vulnerability findings for one target, classified against the baseline,
/// with the coverage that could not be assessed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanReport {
    /// Identifier of the scanned target.
    pub target: String,
    /// Whether a baseline lock existed to compare against.
    pub baseline_available: bool,
    /// `baseline`, or `candidate-only` when no baseline lock exists.
    pub comparison: String,
    /// Status of the vulnerability database snapshot used for both scans.
    pub database: Value,
    /// Every candidate finding; the lists below classify them against a baseline.
    pub findings: Vec<Finding>,
    /// Findings in the candidate but not the baseline.
    pub introduced: Vec<Finding>,
    /// Findings in the baseline but not the candidate.
    pub resolved: Vec<Finding>,
    /// Findings in both.
    pub remaining: Vec<Finding>,
    /// Baseline packages that could not be assessed (unassessed, not clean).
    pub unknown_before: Vec<Package>,
    /// Candidate packages that could not be assessed (unassessed, not clean).
    pub unknown_after: Vec<Package>,
    /// What the findings do and do not establish about applicability.
    pub applicability: String,
    /// Configured suppressions that had expired and were not applied.
    pub expired_suppressions: Vec<Suppression>,
    /// Whether the configured policy accepted the findings.
    pub policy_passed: bool,
}

/// Build a CycloneDX SBOM from an inventory. Packages get an upstream identity
/// from a reviewed mapping, a PyPI artifact URL on files.pythonhosted.org, or
/// an exact GitHub Actions release tag; anything else, and packages without a
/// version, is returned as unassessed instead of being guessed.
///
/// # Errors
///
/// Returns [`Error::Invalid`] for mappings without an unversioned PURL and
/// HTTPS evidence, or with duplicate entries.
pub fn inventory_sbom(
    packages: &[Package],
    mappings: &[IdentityMapping],
) -> Result<(Value, Vec<Package>)> {
    let valid_purl = regex::Regex::new(r"^pkg:[a-z][a-z0-9.+-]*/[A-Za-z0-9._/-]+$").unwrap();
    let mut components = vec![];
    let mut unknown = vec![];
    let mut keys = BTreeSet::new();
    for mapping in mappings {
        if !mapping.evidence.starts_with("https://") || !valid_purl.is_match(&mapping.purl) {
            return Err(Error::Invalid(
                "identity mappings need an unversioned PURL and HTTPS provenance".into(),
            ));
        }
        if !keys.insert((&mapping.ecosystem, &mapping.name)) {
            return Err(Error::Invalid("duplicate identity mapping".into()));
        }
    }
    for (index, package) in packages.iter().enumerate() {
        let mapping = mappings
            .iter()
            .find(|m| m.ecosystem == package.ecosystem && m.name == package.name);
        let identity = if let Some(mapping) = mapping {
            Some((mapping.purl.clone(), mapping.evidence.clone()))
        } else {
            crate::ecosystem::native_identity(package)
        };
        let Some((identity, evidence)) = identity else {
            unknown.push(package.clone());
            continue;
        };
        if package.version.is_empty()
            || package.version == "unknown"
            || !valid_purl.is_match(&identity)
            || package.version.contains(['@', '/', '?', '#', ' '])
        {
            unknown.push(package.clone());
            continue;
        }
        components.push(json!({"type":"library", "bom-ref": format!("package-{index}"),
            "name":package.name, "version":package.version, "purl":format!("{identity}@{}", package.version),
            "properties":[{"name":"depsmith:identity-evidence", "value":evidence},
                {"name":"depsmith:original", "value":serde_json::to_string(package).unwrap()},
                {"name":"depsmith:build-applicability", "value": crate::ecosystem::build_caveat(&package.ecosystem).unwrap_or("upstream package")}]}));
    }
    Ok((
        json!({"bomFormat":"CycloneDX", "specVersion":"1.5", "version":1, "components":components}),
        unknown,
    ))
}
fn findings(output: &str, sbom: &Value) -> Result<Vec<Finding>> {
    let parsed: Value = serde_json::from_str(output)
        .map_err(|_| Error::Operation("scanner returned invalid JSON".into()))?;
    let rows = parsed
        .get("matches")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Operation("scanner output has no matches array".into()))?;
    let mut result = vec![];
    for m in rows {
        let field = |path: &str| {
            m.pointer(path)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| Error::Operation(format!("scanner missing {path}")))
        };
        let purl = field("/artifact/purl")?;
        let components = sbom["components"].as_array().unwrap();
        let matching: Vec<_> = components
            .iter()
            .filter(|c| c["purl"].as_str() == Some(&purl))
            .collect();
        if matching.is_empty() {
            return Err(Error::Operation(
                "scanner finding cannot be traced to an inventory component".into(),
            ));
        }
        for component in matching {
            let original = component["properties"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["name"] == "depsmith:original")
                .and_then(|p| p["value"].as_str())
                .ok_or_else(|| Error::Operation("missing inventory identity".into()))?;
            let package: Package = serde_json::from_str(original)
                .map_err(|_| Error::Operation("invalid inventory identity".into()))?;
            result.push(Finding {
                id: field("/vulnerability/id")?,
                namespace: field("/vulnerability/namespace")?,
                severity: field("/vulnerability/severity")?,
                package: package.name.clone(),
                version: package.version,
                evidence: m["matchDetails"].clone(),
                applicability: crate::ecosystem::build_caveat(&package.ecosystem)
                    .unwrap_or("upstream")
                    .into(),
                artifact: package.artifact.clone(),
                suppression: None,
                identity: format!(
                    "{}:{}:{}",
                    package.ecosystem, package.name, package.platform
                ),
            });
        }
    }
    result.sort_by(|a, b| {
        (&a.id, &a.namespace, &a.identity, &a.version).cmp(&(
            &b.id,
            &b.namespace,
            &b.identity,
            &b.version,
        ))
    });
    result.dedup_by(|a, b| {
        a.id == b.id
            && a.namespace == b.namespace
            && a.identity == b.identity
            && a.version == b.version
    });
    Ok(result)
}
fn key(f: &Finding) -> (&str, &str, &str) {
    (&f.id, &f.namespace, &f.identity)
}
/// Order of a severity name from `negligible` (0) to `critical` (4), ignoring
/// case; `None` for an unknown name.
pub fn severity_rank(severity: &str) -> Option<u8> {
    match severity.to_ascii_lowercase().as_str() {
        "negligible" => Some(0),
        "low" => Some(1),
        "medium" => Some(2),
        "high" => Some(3),
        "critical" => Some(4),
        _ => None,
    }
}
impl Suppression {
    pub(crate) fn validate(&self) -> Result<()> {
        let invalid = |why: &str| Err(Error::Invalid(format!("suppression {:?}: {why}", self.id)));
        if self.id.trim().is_empty() {
            return invalid("an advisory id is required");
        }
        if self.reason.trim().is_empty() {
            return invalid("a documented reason is required");
        }
        if self.package.is_none() && self.target.is_none() {
            return invalid("scope it to a package (ecosystem:name) or a target");
        }
        if let Some(package) = &self.package {
            if !package
                .split_once(':')
                .is_some_and(|(e, n)| !e.is_empty() && !n.is_empty())
            {
                return invalid("package must be ecosystem:name, e.g. pypi:urllib3");
            }
        }
        if let Some(expires) = &self.expires {
            if crate::cutoff::end_of_date(expires).is_none() {
                return invalid("expires must be a YYYY-MM-DD date");
            }
        }
        Ok(())
    }

    fn expired(&self, now: i64) -> bool {
        self.expires
            .as_deref()
            .and_then(crate::cutoff::end_of_date)
            .is_some_and(|end| now >= end)
    }

    fn in_scope(&self, target: &str) -> bool {
        self.target.as_deref().is_none_or(|t| t == target)
    }

    fn covers(&self, target: &str, finding: &Finding) -> bool {
        self.id == finding.id
            && self.in_scope(target)
            && self
                .package
                .as_deref()
                .is_none_or(|p| finding.identity.starts_with(&format!("{p}:")))
    }
}

/// Scan the baseline (`before`, when a baseline lock exists) and the
/// candidate (`after`) with one vulnerability database snapshot, classify the
/// findings, apply suppressions and evaluate the `fail_on` policy. Without a
/// baseline the report is `candidate-only` and nothing is classified as
/// introduced.
///
/// # Errors
///
/// Returns [`Error::Operation`] when the scanner fails, its output is not
/// valid JSON, or a finding cannot be traced to an inventory package, and
/// [`Error::Invalid`] for invalid identity mappings.
pub fn scan_pair(
    target: &str,
    before: Option<&[Package]>,
    after: &[Package],
    options: &UpdateOptions,
    cwd: &Path,
) -> Result<ScanReport> {
    let directory = tempfile::tempdir()?;
    let cache = directory.path().join("db");
    let mut env: Vec<(String, String)> = options.tool_env("grype").to_vec();
    env.extend([
        (
            "GRYPE_DB_CACHE_DIR".into(),
            cache.to_string_lossy().into_owned(),
        ),
        ("GRYPE_DB_AUTO_UPDATE".into(), "false".into()),
    ]);
    run_env(
        &options.tool("grype"),
        &["db".into(), "update".into()],
        cwd,
        options.timeout_seconds,
        &env,
    )?;
    let database: Value = serde_json::from_str(&run_env(
        &options.tool("grype"),
        &["db".into(), "status".into(), "-o".into(), "json".into()],
        cwd,
        options.timeout_seconds,
        &env,
    )?)
    .map_err(|_| Error::Operation("cannot identify vulnerability database snapshot".into()))?;
    let mut unknown_before = vec![];
    let mut baseline = vec![];
    if let Some(before) = before {
        let (sbom, unknown) = inventory_sbom(before, &options.identity_mappings)?;
        unknown_before = unknown;
        let path = directory.path().join("before.cdx.json");
        fs::write(&path, serde_json::to_vec(&sbom).unwrap())?;
        baseline = findings(
            &run_env(
                &options.tool("grype"),
                &[
                    format!("sbom:{}", path.display()),
                    "-o".into(),
                    "json".into(),
                ],
                cwd,
                options.timeout_seconds,
                &env,
            )?,
            &sbom,
        )?;
    }
    let (sbom, unknown_after) = inventory_sbom(after, &options.identity_mappings)?;
    let path = directory.path().join("after.cdx.json");
    fs::write(&path, serde_json::to_vec(&sbom).unwrap())?;
    let candidate = findings(
        &run_env(
            &options.tool("grype"),
            &[
                format!("sbom:{}", path.display()),
                "-o".into(),
                "json".into(),
            ],
            cwd,
            options.timeout_seconds,
            &env,
        )?,
        &sbom,
    )?;
    let now = crate::cutoff::now_ms();
    let (expired, active): (Vec<_>, Vec<_>) = options
        .suppressions
        .iter()
        .filter(|s| s.in_scope(target))
        .partition(|s| s.expired(now));
    let mut candidate = candidate;
    for finding in &mut candidate {
        finding.suppression = active
            .iter()
            .find(|s| s.covers(target, finding))
            .map(|s| (*s).clone());
    }
    let comparison = if before.is_some() {
        "baseline"
    } else {
        "candidate-only"
    };
    let introduced = candidate
        .iter()
        .filter(|f| !baseline.iter().any(|b| key(b) == key(f)))
        .cloned()
        .collect::<Vec<_>>();
    let remaining = candidate
        .iter()
        .filter(|f| baseline.iter().any(|b| key(b) == key(f)))
        .cloned()
        .collect();
    let resolved = baseline
        .iter()
        .filter(|b| !candidate.iter().any(|f| key(f) == key(b)))
        .cloned()
        .collect();
    // Without a baseline nothing can be classified as introduced or resolved.
    let (introduced, resolved, remaining) = if before.is_some() {
        (introduced, resolved, remaining)
    } else {
        (vec![], vec![], vec![])
    };
    let mut policy_passed = true;
    if let Some(threshold) = &options.fail_on {
        let rank = severity_rank(threshold)
            .ok_or_else(|| Error::Invalid("invalid severity threshold".into()))?;
        // "New" is undefined without a baseline, so all findings are assessed.
        let assessed = if options.only_new && before.is_some() {
            &introduced
        } else {
            &candidate
        };
        policy_passed = !assessed.iter().any(|f| {
            f.suppression.is_none()
                && severity_rank(&f.severity).is_none_or(|severity| severity >= rank)
        });
    }
    Ok(ScanReport { target:target.into(), baseline_available:before.is_some(), comparison: comparison.into(), findings: candidate, expired_suppressions: expired.into_iter().cloned().collect(), database, introduced, resolved, remaining,
        unknown_before, unknown_after, applicability:"Upstream advisory matches; conda build/backport applicability is unknown. Unmapped packages are unassessed, not clean.".into(), policy_passed })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn same_advisory_on_a_new_platform_is_introduced() {
        let baseline: Finding = serde_json::from_value(json!({"id":"CVE-test", "namespace":"nvd", "package":"demo", "version":"1", "severity":"High", "evidence":[], "identity":"conda:demo:linux-64", "applicability":"", "artifact":"", "suppression":null})).unwrap();
        let candidate: Finding = serde_json::from_value(json!({"id":"CVE-test", "namespace":"nvd", "package":"demo", "version":"1", "severity":"High", "evidence":[], "identity":"conda:demo:win-64", "applicability":"", "artifact":"", "suppression":null})).unwrap();
        assert_ne!(key(&baseline), key(&candidate));
    }
}

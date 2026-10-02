//! Constraint suggestions and acceptance for every adapter.
//!
//! An adapter reports its [`Declaration`]s, rewrites them from [`Edit`]s and
//! describes where releases are published as an [`AvailabilityConfig`]. The
//! engine does the rest: it finds pins and upper bounds that exclude a newer
//! release (with evidence), checks and applies `--accept`, restyling the
//! requirement using the ecosystem's version scheme or, for a movable
//! reference such as a GitHub Actions tag, asking the adapter for its
//! [`Pin`].
use crate::{
    adapter::Adapter,
    constraint::{parse_accept, Excluded},
    ecosystem,
    process::run,
    Error, Result, Suggestion, Target, UpdateOptions,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// Where a direct dependency and its constraint are written in a target's
/// files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Declaration {
    /// Ecosystem of the package, such as `conda` or `pypi`.
    pub ecosystem: String,
    /// The package name as declared.
    pub package: String,
    /// The declared requirement; empty when the declaration has no version
    /// (for example a local path).
    pub requirement: String,
    /// File holding the declaration, relative to the repository root.
    pub file: PathBuf,
    /// Adapter-defined position within the file, such as a table path.
    pub location: String,
}

/// A replacement requirement for one declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edit {
    /// The declaration to rewrite, as reported by the adapter.
    pub declaration: Declaration,
    /// Its new requirement.
    pub requirement: String,
    /// A comment to write beside the requirement, such as the release a
    /// commit pin stands for; `None` keeps any existing comment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

/// An immutable requirement for a declaration whose requirement can move,
/// such as the commit a release tag points to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pin {
    /// The immutable requirement.
    pub requirement: String,
    /// A comment to write beside it, such as the release it stands for.
    pub comment: Option<String>,
    /// What the pin is, cited in acceptance notes, such as
    /// `commit pin of v4.2.1`.
    pub note: String,
    /// Where the pin was established, cited in suggestions.
    pub evidence: String,
}

/// Where a target's packages are published, per ecosystem, and the release
/// age policy that applies to every lookup.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvailabilityConfig {
    /// Registry of each ecosystem; ecosystems without one get no suggestions.
    pub registries: BTreeMap<String, RegistryConfig>,
    /// A native release-age cutoff (Pixi's `exclude-newer` forms), applied to
    /// both sides of every comparison.
    pub exclude_newer: Option<String>,
}

/// A registry to consult for newer releases, described as data so that the
/// engine builds the client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RegistryConfig {
    /// `pixi search` against the channels of a Pixi manifest.
    PixiSearch {
        /// The manifest, relative to the stage.
        manifest: PathBuf,
    },
    /// Conda channels' sharded repodata (CEP 16), or full repodata for
    /// channels without shards.
    CondaSharded {
        /// Channel URLs, in order.
        channels: Vec<String>,
        /// Platforms (subdirs) to consult; `noarch` is always included.
        platforms: Vec<String>,
    },
    /// A Cargo sparse registry index such as `https://index.crates.io/`.
    CratesSparse {
        /// Index URL, ending in `/`.
        index: String,
    },
    /// PEP 691 JSON Simple API indexes.
    PypiSimple {
        /// Index URLs, in order.
        indexes: Vec<String>,
    },
    /// Releases given inline, for tests and adapter development.
    Fixture {
        /// Releases by package name.
        releases: BTreeMap<String, Vec<FixtureRelease>>,
        /// Packages whose lookup fails.
        failing: Vec<String>,
    },
}

/// One release of a [`RegistryConfig::Fixture`] registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixtureRelease {
    /// The version.
    pub version: String,
    /// Artifact URL cited as evidence.
    pub url: String,
    /// Artifact checksum cited as evidence.
    pub sha256: String,
}

/// How to act on a suggestion for `package`: accept it, or, where the
/// adapter can change constraints itself, review it with an upgrade.
fn review(adapter: &dyn Adapter, package: &str) -> String {
    let mut text = format!(
        "Preserve intentional pins; accept the evidenced release with --accept {package}, or set one with --accept {package}=REQUIREMENT"
    );
    if adapter.spec().capabilities.constraint_changes == crate::adapter::Support::Supported {
        text.push_str(&format!(
            ", or review a newly resolved constraint with --package {package} --upgrade"
        ));
    }
    text.push('.');
    text
}

/// Any version newer than a declared one; restyling it succeeds exactly when
/// the requirement's style is unambiguous.
const PROBE_VERSION: &str = "999999";

/// Newer-release lookups for one target.
pub(crate) struct Lookup<'a> {
    stage: &'a Path,
    options: &'a UpdateOptions,
    config: AvailabilityConfig,
    shards: crate::conda::ShardCache,
}

impl<'a> Lookup<'a> {
    pub(crate) fn new(
        stage: &'a Path,
        options: &'a UpdateOptions,
        config: AvailabilityConfig,
    ) -> Self {
        Self {
            stage,
            options,
            config,
            shards: Default::default(),
        }
    }

    /// Releases the requirement excludes, plus notes for registries that
    /// could not be consulted.
    fn excluded(
        &self,
        ecosystem: &str,
        package: &str,
        requirement: &str,
    ) -> Result<(Vec<Excluded>, Vec<String>)> {
        let cutoff = match &self.config.exclude_newer {
            None => None,
            Some(text) => {
                Some(crate::cutoff::parse(text, crate::cutoff::now_ms()).ok_or_else(|| {
                    Error::Operation(format!(
                        "exclude-newer = {text:?} is not a form this tool understands; availability not established"
                    ))
                })?)
            }
        };
        let policy = self
            .config
            .exclude_newer
            .as_ref()
            .map(|t| format!("exclude-newer {t}"));
        let label = |mut excluded: Vec<Excluded>| {
            for e in &mut excluded {
                e.policy = policy.clone();
            }
            excluded
        };
        let Some(registry) = self.config.registries.get(ecosystem) else {
            return Err(Error::Operation(format!(
                "no registry is configured for {ecosystem}"
            )));
        };
        match registry {
            RegistryConfig::PixiSearch { manifest } => {
                let all = self.pixi_search(manifest, package)?;
                let allowed = self.pixi_search(manifest, &format!("{package} {requirement}"))?;
                Ok((label(conda_excluded(&all, &allowed, cutoff)), vec![]))
            }
            RegistryConfig::PypiSimple { indexes } => {
                let mut pages = vec![];
                let mut failures = vec![];
                for index in indexes {
                    match crate::pypi::fetch(index, package, self.options.timeout_seconds) {
                        Ok(page) => pages.push((index.clone(), page)),
                        Err(error) => {
                            failures.push(format!("availability lookup failed: {index}: {error}"))
                        }
                    }
                }
                crate::pypi::evidence(package, &pages, requirement, cutoff)
                    .map(|evidence| (label(evidence), failures))
            }
            RegistryConfig::CondaSharded {
                channels,
                platforms,
            } => {
                let all = crate::conda::sharded_records(
                    &self.shards,
                    channels,
                    platforms,
                    package,
                    self.options.timeout_seconds,
                )?;
                let allowed = conda_allowed(&all, requirement)?;
                Ok((label(conda_excluded(&all, &allowed, cutoff)), vec![]))
            }
            RegistryConfig::CratesSparse { index } => crate::cargo::sparse_excluded(
                index,
                package,
                requirement,
                self.options.timeout_seconds,
            )
            .map(|excluded| (label(excluded), vec![])),
            RegistryConfig::Fixture { releases, failing } => {
                if failing.iter().any(|f| f == package) {
                    return Ok((
                        vec![],
                        vec![format!("availability lookup failed: fixture: {package}")],
                    ));
                }
                fixture_excluded(ecosystem, releases.get(package), requirement)
                    .map(|excluded| (label(excluded), vec![]))
            }
        }
    }

    fn pixi_search(&self, manifest: &Path, spec: &str) -> Result<serde_json::Value> {
        let args = [
            "search".into(),
            "--json".into(),
            "--manifest-path".into(),
            self.stage.join(manifest).to_string_lossy().into(),
            spec.into(),
        ];
        let output = run(
            &self.options.tool("pixi"),
            &args,
            self.stage,
            self.options.timeout_seconds,
        )?;
        serde_json::from_str(&output)
            .map_err(|e| Error::Operation(format!("unreadable pixi search output: {e}")))
    }
}

/// Whether a conda record predates the release-age cutoff. (Pre-releases are
/// filtered separately and never cited as the newer release.) Undated records
/// cannot be shown eligible; second-resolution timestamps are normalised as
/// conda does (values up to year 9999 in seconds).
fn eligible(record: &serde_json::Value, cutoff: Option<i64>) -> bool {
    let Some(cutoff) = cutoff else {
        return true;
    };
    record["timestamp"]
        .as_i64()
        .map(|t| if t <= 253_402_300_799 { t * 1000 } else { t })
        .is_some_and(|t| t <= cutoff)
}

/// Compare conda search output (subdir -> records) for a package with the
/// output for its declared requirement. Each entry cites the newest record the
/// requirement excludes; empty means no newer release is blocked on any subdir.
pub(crate) fn conda_excluded(
    all: &serde_json::Value,
    allowed: &serde_json::Value,
    cutoff: Option<i64>,
) -> Vec<Excluded> {
    let newest = |records: Option<&serde_json::Value>| -> Option<serde_json::Value> {
        records?
            .as_array()?
            .iter()
            .filter(|r| {
                r["version"]
                    .as_str()
                    .is_some_and(|v| !crate::conda_version::is_prerelease(v))
                    && eligible(r, cutoff)
            })
            .max_by(|a, b| {
                crate::conda_version::compare(
                    a["version"].as_str().unwrap(),
                    b["version"].as_str().unwrap(),
                )
            })
            .cloned()
    };
    let mut evidence = vec![];
    for (subdir, records) in all.as_object().into_iter().flatten() {
        let Some(latest) = newest(Some(records)) else {
            continue;
        };
        let version = latest["version"].as_str().unwrap();
        let allowed_record = newest(allowed.get(subdir));
        let allowed_version = allowed_record.as_ref().and_then(|r| r["version"].as_str());
        if allowed_version.is_none_or(|a| crate::conda_version::compare(version, a).is_gt()) {
            evidence.push(Excluded {
                policy: None,
                scope: subdir.clone(),
                version: version.into(),
                allowed: allowed_version.map(Into::into),
                url: latest["url"].as_str().unwrap_or("unknown artifact").into(),
                sha256: latest["sha256"].as_str().unwrap_or("unknown").into(),
            });
        }
    }
    evidence
}

/// The records of conda search output (subdir -> records) that satisfy a
/// conda version specification; unreadable specifications are an error,
/// never "nothing allowed".
fn conda_allowed(all: &serde_json::Value, requirement: &str) -> Result<serde_json::Value> {
    let mut allowed = serde_json::Map::new();
    for (subdir, records) in all.as_object().into_iter().flatten() {
        let mut kept = vec![];
        for record in records.as_array().into_iter().flatten() {
            let version = record["version"].as_str().unwrap_or_default();
            match crate::conda_version::matches(requirement, version) {
                Some(true) => kept.push(record.clone()),
                Some(false) => {}
                None => {
                    return Err(Error::Operation(format!(
                        "{requirement:?} is not a conda version specification depsmith can evaluate"
                    )))
                }
            }
        }
        allowed.insert(subdir.clone(), kept.into());
    }
    Ok(allowed.into())
}

fn fixture_excluded(
    ecosystem: &str,
    releases: Option<&Vec<FixtureRelease>>,
    requirement: &str,
) -> Result<Vec<Excluded>> {
    use crate::pep440::{satisfies, Version};
    let releases = releases.map(Vec::as_slice).unwrap_or_default();
    if ecosystem == "cargo" {
        let rows: Vec<(semver::Version, &str, &str)> = releases
            .iter()
            .filter_map(|r| {
                Some((
                    semver::Version::parse(&r.version).ok()?,
                    r.url.as_str(),
                    r.sha256.as_str(),
                ))
            })
            .collect();
        return crate::cargo::semver_excluded("fixture", &rows, requirement);
    }
    if ecosystem == "conda" {
        let rows: Vec<serde_json::Value> = releases
            .iter()
            .map(|r| serde_json::json!({"version": r.version, "url": r.url, "sha256": r.sha256}))
            .collect();
        let all = serde_json::json!({ "fixture": rows });
        let allowed = conda_allowed(&all, requirement)?;
        return Ok(conda_excluded(&all, &allowed, None));
    }
    if ecosystem != "pypi" {
        return Err(Error::Operation(format!(
            "fixture registries match PEP 440, Cargo or conda requirements only, not {ecosystem}"
        )));
    }
    let finals: Vec<_> = releases
        .iter()
        .filter_map(|r| Version::parse(&r.version).map(|v| (v, r)))
        .filter(|(v, _)| !v.is_prerelease())
        .collect();
    let Some((newest, record)) = finals.iter().max_by(|a, b| a.0.cmp(&b.0)) else {
        return Ok(vec![]);
    };
    let allowed = finals
        .iter()
        .filter(|(v, _)| satisfies(requirement, v) == Some(true))
        .map(|(v, _)| v)
        .max();
    if allowed.is_some_and(|a| a >= newest) {
        return Ok(vec![]);
    }
    Ok(vec![Excluded {
        policy: None,
        scope: "fixture".into(),
        version: record.version.clone(),
        allowed: allowed.map(|v| v.text().to_owned()),
        url: record.url.clone(),
        sha256: record.sha256.clone(),
    }])
}

/// Whether `declared` names the package `requested`: PyPI names compare after
/// PEP 503 normalisation, other ecosystems case-insensitively.
fn same_package(ecosystem: &str, declared: &str, requested: &str) -> bool {
    if ecosystem == "pypi" {
        crate::adapter::pypi_key(declared) == crate::adapter::pypi_key(requested)
    } else {
        declared.eq_ignore_ascii_case(requested)
    }
}

/// Suggestions for the target's declarations: pins and upper bounds that
/// exclude a newer release on its registries. Failed lookups stay as
/// unestablished suggestions, never as clean results.
pub(crate) fn suggest(
    adapter: &dyn Adapter,
    stage: &Path,
    target: &Target,
    lookup: &Lookup,
) -> Result<Vec<Suggestion>> {
    let mut seen = vec![];
    let mut suggestions = vec![];
    for declaration in adapter.declarations(stage, target)? {
        let Some(scheme) = ecosystem::scheme(&declaration.ecosystem) else {
            continue;
        };
        if declaration.requirement.is_empty()
            || !scheme.caps_newer(&declaration.requirement)
            || !lookup
                .config
                .registries
                .contains_key(&declaration.ecosystem)
        {
            continue;
        }
        let key = (
            declaration.ecosystem.clone(),
            declaration.package.clone(),
            declaration.requirement.clone(),
        );
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        let review = review(adapter, &declaration.package);
        let unestablished = format!("Declared pin or upper bound can exclude newer releases. {review} Newer availability has not been established.");
        let (reason, evidence) = match lookup.excluded(
            &declaration.ecosystem,
            &declaration.package,
            &declaration.requirement,
        ) {
            Ok((excluded, failures)) if excluded.is_empty() && failures.is_empty() => continue,
            Ok((excluded, failures)) if excluded.is_empty() => (unestablished, failures),
            Ok((excluded, failures)) => (
                format!("Declared pin or upper bound excludes a newer release. {review}"),
                excluded
                    .iter()
                    .map(ToString::to_string)
                    .chain(failures)
                    .collect(),
            ),
            Err(error) => (
                unestablished,
                vec![format!("availability lookup failed: {error}")],
            ),
        };
        suggestions.push(Suggestion {
            target: target.id.clone(),
            package: declaration.package,
            requirement: declaration.requirement,
            reason,
            evidence,
        });
    }
    Ok(suggestions)
}

/// Apply `--accept` (`NAME` or `NAME=REQUIREMENT`, names as declared) to the
/// target in the stage. Every acceptance is checked before any lookup, so
/// ambiguous styles or malformed explicit requirements fail without network
/// access. Returns validation notes.
pub(crate) fn accept(
    adapter: &dyn Adapter,
    stage: &Path,
    target: &Target,
    accepted: &[String],
    lookup: &Lookup,
) -> Result<Vec<String>> {
    if accepted.is_empty() {
        return Ok(vec![]);
    }
    let acceptances = accepted
        .iter()
        .map(|a| parse_accept(a))
        .collect::<Result<Vec<_>>>()?;
    let declarations = adapter.declarations(stage, target)?;
    let mut plans = vec![];
    for acceptance in &acceptances {
        let name = &acceptance.name;
        let matching: Vec<&Declaration> = declarations
            .iter()
            .filter(|d| same_package(&d.ecosystem, &d.package, name))
            .collect();
        if matching.is_empty() {
            return Err(Error::Invalid(format!(
                "{name} is not a declared dependency"
            )));
        }
        let versioned: Vec<&Declaration> = matching
            .into_iter()
            .filter(|d| !d.requirement.is_empty())
            .collect();
        if versioned.is_empty() {
            return Err(Error::Invalid(format!(
                "{name} is declared with no version requirement to accept"
            )));
        }
        for declaration in &versioned {
            let scheme = ecosystem::scheme(&declaration.ecosystem).ok_or_else(|| {
                Error::Invalid(format!(
                    "{name}: no version scheme for the {} ecosystem",
                    declaration.ecosystem
                ))
            })?;
            let old = &declaration.requirement;
            match &acceptance.requirement {
                Some(requirement) => scheme.check_explicit(name, requirement)?,
                None if scheme.pinned(old) == Some(true) => {
                    return Err(Error::Invalid(format!(
                        "{name} {old} is already a commit pin; nothing to accept"
                    )))
                }
                None if scheme.pinned(old) == Some(false) => {}
                None if scheme.restyle(old, PROBE_VERSION).is_some() => {}
                None => {
                    return Err(Error::Invalid(format!(
                        "{name} {old}: the requirement style is ambiguous; pass --accept {name}=REQUIREMENT"
                    )))
                }
            }
        }
        plans.push((acceptance, versioned));
    }
    let mut edits = vec![];
    let mut notes = vec![];
    for (acceptance, versioned) in plans {
        let name = &acceptance.name;
        for declaration in versioned {
            let old = &declaration.requirement;
            let scheme = ecosystem::scheme(&declaration.ecosystem).unwrap();
            let mut comment = None;
            let (new, cited) = match &acceptance.requirement {
                Some(requirement) => (requirement.clone(), "explicit replacement".to_owned()),
                None if scheme.pinned(old) == Some(false) => {
                    let pin = adapter.pin(stage, declaration, lookup.options)?;
                    comment = pin.comment;
                    (pin.requirement, pin.note)
                }
                None => {
                    let (excluded, failures) =
                        lookup.excluded(&declaration.ecosystem, name, old)?;
                    let newest = excluded
                        .iter()
                        .max_by(|a, b| scheme.compare(&a.version, &b.version))
                        .ok_or_else(|| {
                            if failures.is_empty() {
                                Error::Invalid(format!(
                                    "{name} {old}: no newer release is excluded; nothing to accept"
                                ))
                            } else {
                                Error::Operation(format!(
                                    "{name} {old}: newer availability could not be established: {}",
                                    failures.join("; ")
                                ))
                            }
                        })?;
                    let new = scheme.restyle(old, &newest.version).ok_or_else(|| {
                        Error::Invalid(format!(
                            "{name} {old}: cannot restyle to {}; pass --accept {name}=REQUIREMENT",
                            newest.version
                        ))
                    })?;
                    (new, newest.to_string())
                }
            };
            notes.push(format!(
                "{}: accepted {name} {old} -> {new} ({cited})",
                target.id
            ));
            edits.push(Edit {
                declaration: declaration.clone(),
                requirement: new,
                comment,
            });
        }
    }
    adapter.rewrite(stage, target, &edits)?;
    Ok(notes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An adapter that only declares whether it can change constraints.
    struct Changes(crate::adapter::Support);
    impl Adapter for Changes {
        fn spec(&self) -> crate::adapter::AdapterSpec {
            let mut spec = crate::adapter::AdapterSpec::new("changes", &[]);
            spec.capabilities.constraint_changes = self.0.clone();
            spec
        }
        fn detects(&self, _: &Path, _: &str) -> bool {
            false
        }
        fn prepare(
            &self,
            _: &Path,
            _: &Target,
            _: &crate::UpdateOptions,
        ) -> Result<crate::adapter::Candidate> {
            unreachable!()
        }
    }

    #[test]
    fn hints_offer_upgrade_only_where_constraints_can_change() {
        use crate::adapter::Support;
        let upgrading = review(&Changes(Support::Supported), "six");
        assert!(
            upgrading.contains("--accept six")
                && upgrading.contains("--accept six=REQUIREMENT")
                && upgrading.contains("--package six --upgrade"),
            "{upgrading}"
        );
        let accepting = review(&Changes(Support::Unsupported("use --accept".into())), "six");
        assert!(
            accepting.contains("--accept six") && !accepting.contains("--upgrade"),
            "{accepting}"
        );
    }

    fn records(versions: &[(&str, &[&str])]) -> serde_json::Value {
        let map: serde_json::Map<_, _> = versions
            .iter()
            .map(|(subdir, list)| {
                let rows = list
                    .iter()
                    .map(|v| serde_json::json!({"version": v, "url": format!("https://example.invalid/{subdir}/pkg-{v}.conda"), "sha256": format!("sha-{v}")}))
                    .collect();
                (subdir.to_string(), serde_json::Value::Array(rows))
            })
            .collect();
        serde_json::Value::Object(map)
    }

    #[test]
    fn evidence_names_newest_excluded_record_per_subdir() {
        // Unordered on purpose: availability must not depend on backend ordering.
        let all = records(&[
            ("linux-64", &["0.15.22", "0.16.9", "0.9.0"]),
            ("win-64", &["0.15.22"]),
            ("noarch", &["2.39"]),
        ]);
        let allowed = records(&[("linux-64", &["0.15.22"]), ("win-64", &["0.15.22"])]);
        let evidence: Vec<String> = conda_excluded(&all, &allowed, None)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            evidence,
            [
                "linux-64: 0.16.9 is excluded (newest allowed 0.15.22): https://example.invalid/linux-64/pkg-0.16.9.conda sha256:sha-0.16.9",
                "noarch: 2.39 is excluded (newest allowed none): https://example.invalid/noarch/pkg-2.39.conda sha256:sha-2.39",
            ]
        );
        assert!(conda_excluded(&allowed, &allowed, None).is_empty());
    }

    #[test]
    fn release_age_cutoff_limits_candidates_on_both_sides() {
        let record = |v: &str, ts: Option<i64>| {
            let mut r = serde_json::json!({"version": v, "url": format!("u-{v}"), "sha256": "x"});
            if let Some(ts) = ts {
                r["timestamp"] = ts.into();
            }
            r
        };
        let all = serde_json::json!({"linux-64": [
            record("0.15.22", Some(1_000_000_000_000)),
            record("0.16.0", Some(2_000_000_000)), // seconds, as in older repodata
            record("0.16.9", Some(3_000_000_000_000)),
            record("0.17.0", None),
        ]});
        let allowed = serde_json::json!({"linux-64": [record("0.15.22", Some(1_000_000_000_000))]});
        let versions = |cutoff| -> Vec<String> {
            conda_excluded(&all, &allowed, cutoff)
                .into_iter()
                .map(|e| e.version)
                .collect()
        };
        assert_eq!(versions(None), ["0.17.0"]);
        // Pre-releases are never cited as the newer release.
        let with_rc = serde_json::json!({"linux-64": [
            record("0.15.22", Some(1_000_000_000_000)),
            record("0.18.0rc1", Some(1_000_000_000_000)),
        ]});
        assert!(conda_excluded(&with_rc, &allowed, None).is_empty());
        assert_eq!(versions(Some(2_500_000_000_000)), ["0.16.0"]);
        assert!(versions(Some(1_500_000_000_000)).is_empty());
    }
}

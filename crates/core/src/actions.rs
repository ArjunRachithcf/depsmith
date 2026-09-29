use crate::{
    adapter::{Adapter, Candidate, Capabilities, Support},
    Error, Result, Suggestion, Target, UpdateOptions,
};
use regex::Regex;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_yaml::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    time::Duration,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Release {
    pub tag: String,
    pub sha: String,
}
fn version(tag: &str) -> Option<Version> {
    let plain = tag.strip_prefix('v').unwrap_or(tag);
    let count = plain.split('.').count();
    let padded = match count {
        1 => format!("{plain}.0.0"),
        2 => format!("{plain}.0"),
        _ => plain.into(),
    };
    Version::parse(&padded).ok().filter(|v| v.pre.is_empty())
}
fn remote(value: &str) -> Option<(&str, &str)> {
    let (path, reference) = value.rsplit_once('@')?;
    if path.starts_with('.') || path.starts_with("docker:") || path.contains("${{") {
        return None;
    }
    let mut parts = path.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((&path[..owner.len() + repo.len() + 1], reference))
}
fn refs(value: &Value) -> Vec<String> {
    let mut refs = vec![];
    if let Some(jobs) = value.get("jobs").and_then(Value::as_mapping) {
        for job in jobs.values() {
            if let Some(reference) = job.get("uses").and_then(Value::as_str) {
                refs.push(reference.into());
            }
            if let Some(steps) = job.get("steps").and_then(Value::as_sequence) {
                for step in steps {
                    if let Some(reference) = step.get("uses").and_then(Value::as_str) {
                        refs.push(reference.into());
                    }
                }
            }
        }
    }
    refs
}
fn mutate_refs(value: &mut Value, replacements: &BTreeMap<String, String>) {
    if let Some(jobs) = value.get_mut("jobs").and_then(Value::as_mapping_mut) {
        for job in jobs.values_mut() {
            if let Some(slot) = job.get_mut("uses") {
                if let Some(new) = slot.as_str().and_then(|s| replacements.get(s)) {
                    *slot = Value::String(new.clone());
                }
            }
            if let Some(steps) = job.get_mut("steps").and_then(Value::as_sequence_mut) {
                for step in steps {
                    if let Some(slot) = step.get_mut("uses") {
                        if let Some(new) = slot.as_str().and_then(|s| replacements.get(s)) {
                            *slot = Value::String(new.clone());
                        }
                    }
                }
            }
        }
    }
}

pub fn rewrite(
    source: &str,
    repository: &str,
    releases: &[Release],
    major: bool,
) -> Result<String> {
    rewrite_explained(source, repository, releases, major).map(|(text, _)| text)
}

/// Like [`rewrite`], also returning `(reference, reason)` for each reference to
/// `repository` left unchanged because it could not be resolved. References that
/// are already current are not reported.
pub fn rewrite_explained(
    source: &str,
    repository: &str,
    releases: &[Release],
    major: bool,
) -> Result<(String, Vec<(String, String)>)> {
    let mut unresolved = BTreeMap::new();
    let mut expected: Value = serde_yaml::from_str(source)
        .map_err(|e| Error::Invalid(format!("invalid workflow YAML: {e}")))?;
    let mut replacements = BTreeMap::new();
    let mut labels = BTreeMap::new();
    // Tag refs whose trailing `# <old tag>` comment must follow the update.
    let mut retagged: BTreeMap<String, (String, String)> = BTreeMap::new();
    for old in refs(&expected) {
        let Some((repo, reference)) = remote(&old) else {
            continue;
        };
        if repo != repository {
            continue;
        }
        let pinned = reference.len() == 40 && reference.chars().all(|c| c.is_ascii_hexdigit());
        let baseline = if pinned {
            releases
                .iter()
                .filter(|r| r.sha == reference)
                .filter_map(|r| version(&r.tag))
                .max()
        } else {
            version(reference)
        };
        let Some(baseline) = baseline else {
            let reason = if pinned {
                format!(
                    "commit {reference} does not match any release tag, so its version is unknown"
                )
            } else {
                format!("`{reference}` is not a version tag or commit SHA; pin a release tag or commit to receive updates")
            };
            unresolved.insert(old, reason);
            continue;
        };
        let next = releases
            .iter()
            .filter_map(|r| version(&r.tag).map(|v| (v, r)))
            .filter(|(v, _)| *v >= baseline && (major || v.major == baseline.major))
            .max_by(|a, b| a.0.cmp(&b.0));
        let Some((new_version, release)) = next else {
            unresolved.insert(
                old,
                format!(
                    "no release at or above {baseline} in major {} was found among published releases",
                    baseline.major
                ),
            );
            continue;
        };
        if pinned
            && (release.sha.len() != 40 || !release.sha.chars().all(|c| c.is_ascii_hexdigit()))
        {
            return Err(Error::Operation("invalid release commit SHA".into()));
        }
        let next_ref = if pinned {
            release.sha.clone()
        } else {
            let prefix = if reference.starts_with('v') { "v" } else { "" };
            match reference.trim_start_matches('v').split('.').count() {
                1 => format!("{prefix}{}", new_version.major),
                2 => format!("{prefix}{}.{}", new_version.major, new_version.minor),
                _ => release.tag.clone(),
            }
        };
        if !pinned && !releases.iter().any(|r| r.tag == next_ref) && next_ref != reference {
            unresolved.insert(
                old,
                format!(
                    "tag {next_ref} does not exist for release {}; the reference's alias style cannot be preserved",
                    release.tag
                ),
            );
            continue;
        }
        let path = old.rsplit_once('@').unwrap().0;
        let new = format!("{path}@{next_ref}");
        if new != old {
            if pinned {
                labels.insert(old.clone(), release.tag.clone());
            } else {
                retagged.insert(old.clone(), (reference.to_owned(), next_ref.clone()));
            }
            replacements.insert(old, new);
        }
    }
    mutate_refs(&mut expected, &replacements);
    let pattern = Regex::new(r#"^(\s*(?:-\s+)?uses:\s*)(['"]?)([^\s'"]+)(['"]?)(.*)$"#).unwrap();
    let block = Regex::new(r":\s*[|>][+-]?[0-9]?\s*(?:#.*)?$").unwrap();
    let mut block_indent: Option<usize> = None;
    let mut result = String::new();
    for line in source.split_inclusive('\n') {
        let ending = if line.ends_with("\r\n") {
            "\r\n"
        } else if line.ends_with('\n') {
            "\n"
        } else {
            ""
        };
        let text = line.trim_end_matches(['\r', '\n']);
        let indent = text.len() - text.trim_start().len();
        if let Some(depth) = block_indent {
            if text.trim().is_empty() || indent > depth {
                result.push_str(line);
                continue;
            }
            block_indent = None;
        }
        if block.is_match(text) {
            block_indent = Some(indent);
        }
        if let Some(captures) = pattern.captures(text) {
            if let Some(new) = replacements.get(&captures[3]) {
                let suffix = if let Some(label) = labels.get(&captures[3]) {
                    format!(" # {label}")
                } else if let Some((from, to)) = retagged.get(&captures[3]).filter(|(from, _)| {
                    captures[5].trim_start().strip_prefix('#').map(str::trim) == Some(from.as_str())
                }) {
                    captures[5].replacen(from.as_str(), to, 1)
                } else {
                    captures[5].to_string()
                };
                result.push_str(&format!(
                    "{}{}{}{}{}{}",
                    &captures[1], &captures[2], new, &captures[4], suffix, ending
                ));
                continue;
            }
        }
        result.push_str(line);
    }
    let actual: Value =
        serde_yaml::from_str(&result).map_err(|e| Error::Operation(e.to_string()))?;
    if actual != expected {
        return Err(Error::Operation(
            "cannot edit this YAML layout without changing unrelated content".into(),
        ));
    }
    Ok((result, unresolved.into_iter().collect()))
}

pub trait ReleaseSource: Send + Sync {
    fn releases(&self, repository: &str, timeout: u64) -> Result<Vec<Release>>;
}
pub struct GitHub;
impl ReleaseSource for GitHub {
    fn releases(&self, repository: &str, timeout: u64) -> Result<Vec<Release>> {
        if !repository.split('/').all(|p| {
            !p.is_empty()
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        }) {
            return Err(Error::Invalid("invalid GitHub repository".into()));
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(timeout))
            .user_agent("depsmith/0.1")
            .build()
            .map_err(|_| Error::Operation("cannot construct GitHub client".into()))?;
        let mut tags = BTreeMap::new();
        let mut stable = BTreeSet::new();
        for endpoint in ["tags", "releases"] {
            for page in 1..=20 {
                let mut request = client.get(format!(
                    "https://api.github.com/repos/{repository}/{endpoint}?per_page=100&page={page}"
                ));
                if let Ok(token) =
                    std::env::var("GITHUB_TOKEN").or_else(|_| std::env::var("GH_TOKEN"))
                {
                    request = request.bearer_auth(token);
                }
                let response = request
                    .send()
                    .map_err(|_| Error::Operation("GitHub request failed".into()))?;
                if !response.status().is_success() {
                    return Err(Error::Operation(format!(
                        "GitHub returned {}; check authentication/rate limits",
                        response.status()
                    )));
                }
                let data: Vec<serde_json::Value> = response
                    .json()
                    .map_err(|_| Error::Operation("invalid GitHub response".into()))?;
                for row in &data {
                    if endpoint == "tags" {
                        if let (Some(name), Some(sha)) =
                            (row["name"].as_str(), row["commit"]["sha"].as_str())
                        {
                            tags.insert(name.to_owned(), sha.to_owned());
                        }
                    } else if row["draft"] == false && row["prerelease"] == false {
                        if let Some(tag) = row["tag_name"].as_str() {
                            stable.insert(tag.to_owned());
                        }
                    }
                }
                if data.len() < 100 {
                    break;
                }
                if page == 20 {
                    return Err(Error::Operation("GitHub history exceeds pagination limit; refusing incomplete release selection".into()));
                }
            }
        }
        let stable_commits: BTreeSet<_> = stable
            .iter()
            .filter_map(|tag| tags.get(tag).cloned())
            .collect();
        Ok(tags
            .into_iter()
            .filter(|(tag, sha)| stable.contains(tag) || stable_commits.contains(sha))
            .map(|(tag, sha)| Release { tag, sha })
            .collect())
    }
}
pub struct Actions {
    pub source: Box<dyn ReleaseSource>,
}
impl Default for Actions {
    fn default() -> Self {
        Self {
            source: Box::new(GitHub),
        }
    }
}
impl Adapter for Actions {
    fn manager(&self) -> &'static str {
        "github-actions"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            manager: self.manager(),
            package_selection: Support::Supported,
            // Major-version upgrades of selected repositories.
            constraint_changes: Support::Supported,
            suggestion_acceptance: Support::Unsupported(
                "use --upgrade --package OWNER/REPO to review a major release",
            ),
            git_refresh: Support::NotApplicable,
            cooldown: Support::Unsupported(
                "a release-age cooldown is not implemented for GitHub Actions",
            ),
            install_validation: Support::NotApplicable,
            lockfile: Support::NotApplicable,
            platforms: Support::NotApplicable,
            native_tool: None,
        }
    }
    fn detects(&self, path: &Path, _: &str) -> bool {
        let parts: Vec<_> = path.components().collect();
        parts
            .windows(2)
            .any(|p| p[0].as_os_str() == ".github" && p[1].as_os_str() == "workflows")
            && matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("yml" | "yaml")
            )
    }
    fn select(
        &self,
        root: &Path,
        target: &Target,
        requested: &[String],
    ) -> Result<BTreeMap<String, String>> {
        let parsed: Value = serde_yaml::from_str(&fs::read_to_string(root.join(&target.manifest))?)
            .map_err(|e| Error::Invalid(e.to_string()))?;
        let repositories: BTreeSet<_> = refs(&parsed)
            .iter()
            .filter_map(|r| remote(r).map(|(repo, _)| repo.to_owned()))
            .collect();
        Ok(requested
            .iter()
            .filter_map(|request| {
                repositories
                    .iter()
                    .find(|repo| repo.eq_ignore_ascii_case(request.trim()))
                    .map(|repo| (request.clone(), repo.clone()))
            })
            .collect())
    }
    fn inventory(&self, root: &Path, target: &Target) -> Result<Vec<crate::Package>> {
        workflow_inventory(&fs::read_to_string(root.join(&target.manifest))?)
    }
    fn prepare(&self, stage: &Path, target: &Target, options: &UpdateOptions) -> Result<Candidate> {
        let file = stage.join(&target.manifest);
        let mut content = fs::read_to_string(&file)?;
        let before = workflow_inventory(&content)?;
        let parsed: Value =
            serde_yaml::from_str(&content).map_err(|e| Error::Invalid(e.to_string()))?;
        let repositories: BTreeSet<_> = refs(&parsed)
            .iter()
            .filter_map(|r| remote(r).map(|(repo, _)| repo.to_owned()))
            .collect();
        let mut suggestions = vec![];
        let mut unresolved = vec![];
        for repository in repositories {
            if !options.packages.is_empty() && !options.packages.contains(&repository) {
                continue;
            }
            let releases = self.source.releases(&repository, options.timeout_seconds)?;
            let (newer, explanations) =
                rewrite_explained(&content, &repository, &releases, options.upgrade)?;
            unresolved.extend(explanations.into_iter().map(|(reference, reason)| {
                crate::Unresolved {
                    target: target.id.clone(),
                    package: repository.clone(),
                    reference,
                    reason,
                }
            }));
            if rewrite(&content, &repository, &releases, true)? != newer {
                suggestions.push(Suggestion { target: target.id.clone(), package: repository.clone(), requirement: "current major".into(), reason: "A newer major release is available; select this repository with --upgrade to review it.".into(), evidence: vec![format!("GitHub releases: https://github.com/{repository}/releases")] });
            }
            content = newer;
        }
        let after = workflow_inventory(&content)?;
        fs::write(&file, content)?;
        Ok(Candidate {
            files: vec![target.manifest.clone()],
            suggestions,
            unresolved,
            before,
            after,
            baseline_available: true,
            validation: vec![format!(
                "{}: YAML changes confined to remote uses references",
                target.id
            )],
        })
    }
}

pub fn workflow_inventory(content: &str) -> Result<Vec<crate::Package>> {
    let parsed: Value = serde_yaml::from_str(content).map_err(|e| Error::Invalid(e.to_string()))?;
    Ok(refs(&parsed)
        .iter()
        .filter_map(|r| {
            remote(r).map(|(repo, reference)| crate::Package {
                ecosystem: "github-actions".into(),
                name: repo.into(),
                version: reference.into(),
                artifact: format!("https://github.com/{repo}@{reference}"),
                platform: "workflow".into(),
            })
        })
        .collect())
}

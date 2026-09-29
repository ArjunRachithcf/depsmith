//! The Pixi adapter: resolves `pixi.toml` and Pixi-managed `pyproject.toml`
//! targets with Pixi inside the stage, preserves Git pins, reports constraint
//! suggestions with availability evidence, and rewrites accepted constraints.
use crate::constraint::Excluded;
use crate::{
    adapter::{pypi_key, Adapter, Candidate, Capabilities, NativeTool, Support},
    process::run,
    Error, Result, Suggestion, Target, UpdateOptions,
};
use std::{collections::BTreeMap, fs, path::Path};

/// The Pixi adapter. Targets are `pixi.toml` files and `pyproject.toml` files
/// with a `[tool.pixi]` table; Pixi itself resolves in the stage.
pub struct Pixi;
impl Adapter for Pixi {
    fn manager(&self) -> &'static str {
        "pixi"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            manager: self.manager(),
            package_selection: Support::Supported,
            constraint_changes: Support::Supported,
            suggestion_acceptance: Support::Supported,
            git_refresh: Support::Supported,
            cooldown: Support::Unsupported(
                "a common cooldown is not implemented; configure workspace.exclude-newer in the Pixi manifest",
            ),
            install_validation: Support::Supported,
            lockfile: Support::Supported,
            platforms: Support::Supported,
            native_tool: Some(NativeTool {
                option: "pixi",
                tested_versions: &["0.80.0"],
            }),
        }
    }
    fn detects(&self, path: &Path, content: &str) -> bool {
        path.file_name().is_some_and(|n| n == "pixi.toml")
            || (path.file_name().is_some_and(|n| n == "pyproject.toml")
                && content
                    .parse::<toml::Value>()
                    .ok()
                    .and_then(|v| v.get("tool")?.get("pixi").cloned())
                    .is_some())
    }
    fn inventory(&self, root: &Path, target: &Target) -> Result<Vec<crate::Package>> {
        let relative = target.manifest.parent().unwrap().join("pixi.lock");
        let text = match fs::read_to_string(root.join(&relative)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::Invalid(format!(
                    "{}: no {} to scan; create it with `pixi lock` or `depsmith update`",
                    target.id,
                    relative.display()
                )))
            }
            result => result?,
        };
        crate::inventory::pixi_inventory(&text)
    }
    fn select(
        &self,
        root: &Path,
        target: &Target,
        requested: &[String],
    ) -> Result<BTreeMap<String, String>> {
        let parsed: toml::Value = fs::read_to_string(root.join(&target.manifest))?
            .parse()
            .map_err(|e| Error::Invalid(format!("invalid manifest: {e}")))?;
        let mut declared = vec![];
        if target
            .manifest
            .file_name()
            .is_some_and(|n| n == "pyproject.toml")
        {
            if let Some(pixi) = parsed.get("tool").and_then(|t| t.get("pixi")) {
                dependency_names(pixi, &mut declared);
            }
            project_names(&parsed, &mut declared);
        } else {
            dependency_names(&parsed, &mut declared);
        }
        let mut selected = BTreeMap::new();
        for request in requested {
            let found = declared.iter().find(|(name, pypi)| {
                if *pypi {
                    pypi_key(name) == pypi_key(request)
                } else {
                    name.eq_ignore_ascii_case(request.trim())
                }
            });
            if let Some((name, _)) = found {
                selected.insert(request.clone(), name.clone());
            }
        }
        Ok(selected)
    }
    fn prepare(&self, stage: &Path, target: &Target, options: &UpdateOptions) -> Result<Candidate> {
        let manifest = stage.join(&target.manifest);
        let content = fs::read_to_string(&manifest)?;
        let parsed: toml::Value = content
            .parse()
            .map_err(|e| Error::Invalid(format!("invalid manifest: {e}")))?;
        validate_paths(&parsed, manifest.parent().unwrap(), stage)?;
        for relative in crate::working_tree::files(stage)? {
            if relative
                .file_name()
                .is_some_and(|n| n == "pyproject.toml" || n == "pixi.toml")
            {
                let path = stage.join(relative);
                let value: toml::Value = fs::read_to_string(&path)?
                    .parse()
                    .map_err(|e| Error::Invalid(format!("invalid staged manifest: {e}")))?;
                validate_paths(&value, path.parent().unwrap(), stage)?;
            }
        }
        if options.upgrade && options.packages.is_empty() {
            return Err(Error::Invalid(
                "constraint changes require explicitly selected direct packages".into(),
            ));
        }
        let pyproject = target.manifest.file_name().unwrap() == "pyproject.toml";
        let pixi = if pyproject {
            parsed.get("tool").and_then(|v| v.get("pixi")).unwrap()
        } else {
            &parsed
        };
        let availability = Availability {
            manifest: &manifest,
            stage,
            options,
            indexes: crate::pypi::index_urls(pixi),
            exclude_newer: ["workspace", "project"]
                .iter()
                .find_map(|table| pixi.get(table)?.get("exclude-newer"))
                .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned)),
        };
        let (content, accepted) =
            accept_suggestions(&content, pyproject, target, options, &availability)?;
        if !accepted.is_empty() {
            fs::write(&manifest, &content)?;
        }
        // Suggestions describe the manifest as it will be after acceptance.
        let edited: toml::Value = content
            .parse()
            .map_err(|e| Error::Invalid(format!("invalid manifest: {e}")))?;
        let constraints = manifest_constraints(&edited, pyproject);
        let lock = target.manifest.parent().unwrap().join("pixi.lock");
        let before_text = fs::read_to_string(stage.join(&lock)).ok();
        let before = before_text
            .as_deref()
            .map(crate::inventory::pixi_inventory)
            .transpose()?
            .unwrap_or_default();
        // Source metadata can require a solve environment even for a lock-only
        // update. Let Pixi create it inside this disposable stage; forcing
        // --no-install rejects editable/dynamic PyPI dependencies.
        let mut args = vec![
            if options.upgrade { "upgrade" } else { "update" }.into(),
            "--manifest-path".into(),
            manifest.to_string_lossy().into(),
        ];
        args.extend(options.packages.clone());
        run(&options.pixi, &args, stage, options.timeout_seconds)?;
        let after_text = fs::read_to_string(stage.join(&lock))?;
        let after = crate::inventory::pixi_inventory(&after_text)?;
        if !options.refresh_git
            && git_artifacts(&before) != git_artifacts(&after)
            && before_text.is_some()
        {
            return Err(Error::Policy(
                "backend moved Git resolutions; select --refresh-git explicitly".into(),
            ));
        }
        if !options.upgrade && fs::read_to_string(&manifest)? != content {
            return Err(Error::Policy(
                "backend unexpectedly changed the manifest".into(),
            ));
        }
        run(
            &options.pixi,
            &[
                "lock".into(),
                "--manifest-path".into(),
                manifest.to_string_lossy().into(),
                "--check".into(),
            ],
            stage,
            options.timeout_seconds,
        )?;
        if fs::read_to_string(stage.join(&lock))? != after_text {
            return Err(Error::Operation(
                "lock consistency check changed the candidate".into(),
            ));
        }
        let mut validation = accepted;
        validation.push(format!("{}: resolved and lock-consistent", target.id));
        if options.install {
            run(
                &options.pixi,
                &[
                    "install".into(),
                    "--manifest-path".into(),
                    manifest.to_string_lossy().into(),
                    "--locked".into(),
                ],
                stage,
                options.timeout_seconds,
            )?;
            validation.push(format!(
                "{}: default environment installed on host",
                target.id
            ));
        }
        let suggestions = suggest(constraints, target, &availability);
        Ok(Candidate {
            files: vec![target.manifest.clone(), lock],
            suggestions,
            unresolved: vec![],
            before,
            baseline_available: before_text.is_some(),
            after,
            validation,
        })
    }
}
fn git_artifacts(packages: &[crate::Package]) -> std::collections::BTreeSet<String> {
    packages
        .iter()
        .filter(|p| p.artifact.starts_with("git+") || p.artifact.contains(".git"))
        .map(|p| p.artifact.clone())
        .collect()
}
/// Whether a conda or PEP 440 requirement can exclude a newer release. Every `|`
/// alternative needs a `,` clause other than a lower bound or exclusion.
fn caps_newer_releases(requirement: &str) -> bool {
    requirement.split('|').all(|alternative| {
        alternative.split(',').map(str::trim).any(|clause| {
            !(clause.is_empty()
                || clause == "*"
                || clause.starts_with('>')
                || clause.starts_with("!="))
        })
    })
}

/// Compare `pixi search --json` output for a package with the output for its
/// declared requirement. Each entry cites the newest record the requirement
/// excludes; empty means no newer release is blocked on any subdir.
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

fn blocked_evidence(
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

/// Declared direct dependency names as (name, is_pypi), including features and
/// platform targets.
fn dependency_names(value: &toml::Value, output: &mut Vec<(String, bool)>) {
    for (key, val) in value.as_table().into_iter().flatten() {
        match key.as_str() {
            "dependencies" | "host-dependencies" | "build-dependencies" | "pypi-dependencies" => {
                for name in val.as_table().into_iter().flatten().map(|(n, _)| n) {
                    output.push((name.clone(), key == "pypi-dependencies"));
                }
            }
            _ => dependency_names(val, output),
        }
    }
}

/// PEP 508 requirement strings from `[project]` and `[dependency-groups]`.
/// Tables such as `{include-group = ...}` are not requirements.
fn project_requirements(parsed: &toml::Value) -> impl Iterator<Item = &str> {
    let project = parsed.get("project");
    project
        .and_then(|p| p.get("dependencies"))
        .into_iter()
        .chain(
            project
                .and_then(|p| p.get("optional-dependencies"))
                .and_then(|o| o.as_table())
                .into_iter()
                .flat_map(|t| t.values()),
        )
        .chain(
            parsed
                .get("dependency-groups")
                .and_then(|g| g.as_table())
                .into_iter()
                .flat_map(|t| t.values()),
        )
        .filter_map(|l| l.as_array())
        .flatten()
        .filter_map(|r| r.as_str())
}

fn project_names(parsed: &toml::Value, output: &mut Vec<(String, bool)>) {
    for requirement in project_requirements(parsed).filter_map(Pep508::parse) {
        output.push((requirement.name, true));
    }
}

/// The parts of a PEP 508 requirement this tool reads or rewrites.
struct Pep508 {
    name: String,
    /// Byte range of the version specifier, excluding surrounding whitespace
    /// and parentheses; `None` for unversioned and direct-URL requirements.
    specifier: Option<std::ops::Range<usize>>,
}

impl Pep508 {
    fn parse(text: &str) -> Option<Self> {
        let start = text.len() - text.trim_start().len();
        let name_end = text[start..]
            .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
            .map_or(text.len(), |i| start + i);
        if name_end == start {
            return None;
        }
        let mut position =
            name_end + (text[name_end..].len() - text[name_end..].trim_start().len());
        if text[position..].starts_with('[') {
            position += text[position..].find(']')? + 1;
            position += text[position..].len() - text[position..].trim_start().len();
        }
        let rest = &text[position..];
        let range = if rest.starts_with('@') {
            None
        } else if let Some(inner) = rest.strip_prefix('(') {
            Some(position + 1..position + 1 + inner.find(')')?)
        } else {
            Some(position..position + rest.find(';').unwrap_or(rest.len()))
        };
        let specifier = range.and_then(|r| {
            let raw = &text[r.clone()];
            let begin = r.start + (raw.len() - raw.trim_start().len());
            let end = r.start + raw.trim_end().len();
            (begin < end).then_some(begin..end)
        });
        Some(Self {
            name: text[start..name_end].to_owned(),
            specifier,
        })
    }
}

/// Constraints declared in the manifest, including `[project]` and
/// `[dependency-groups]` requirements of a `pyproject.toml` target.
fn manifest_constraints(parsed: &toml::Value, pyproject: bool) -> Vec<Constraint> {
    let mut output = vec![];
    if !pyproject {
        collect_constraints(parsed, &mut output);
        return output;
    }
    if let Some(pixi) = parsed.get("tool").and_then(|v| v.get("pixi")) {
        collect_constraints(pixi, &mut output);
    }
    for text in project_requirements(parsed) {
        let Some(requirement) = Pep508::parse(text) else {
            continue;
        };
        let Some(range) = requirement.specifier else {
            continue;
        };
        let constraint = Constraint {
            package: requirement.name,
            requirement: text[range].to_owned(),
            pypi: true,
        };
        if caps_newer_releases(&constraint.requirement) && !output.contains(&constraint) {
            output.push(constraint);
        }
    }
    output
}

#[derive(Debug, PartialEq, Eq)]
struct Constraint {
    package: String,
    requirement: String,
    pypi: bool,
}

fn collect_constraints(value: &toml::Value, output: &mut Vec<Constraint>) {
    if let Some(table) = value.as_table() {
        for (key, val) in table {
            if matches!(
                key.as_str(),
                "dependencies" | "pypi-dependencies" | "host-dependencies" | "build-dependencies"
            ) {
                if let Some(deps) = val.as_table() {
                    for (name, spec) in deps {
                        let version = spec
                            .as_str()
                            .or_else(|| spec.get("version").and_then(|s| s.as_str()));
                        if let Some(version) = version.filter(|v| caps_newer_releases(v)) {
                            let constraint = Constraint {
                                package: name.clone(),
                                requirement: version.into(),
                                pypi: key == "pypi-dependencies",
                            };
                            if !output.contains(&constraint) {
                                output.push(constraint);
                            }
                        }
                    }
                }
            } else {
                collect_constraints(val, output);
            }
        }
    }
}

const REVIEW: &str =
    "Preserve intentional pins; select this direct package with --upgrade to review a newly resolved constraint.";

/// Newer-release lookups against the staged manifest's conda channels (via
/// `pixi search`) and PyPI indexes.
struct Availability<'a> {
    manifest: &'a Path,
    stage: &'a Path,
    options: &'a UpdateOptions,
    indexes: Vec<String>,
    /// The manifest's native release-age policy, applied to every lookup.
    exclude_newer: Option<String>,
}

impl Availability<'_> {
    fn search(&self, spec: &str) -> Result<serde_json::Value> {
        let args = [
            "search".into(),
            "--json".into(),
            "--manifest-path".into(),
            self.manifest.to_string_lossy().into(),
            spec.into(),
        ];
        let output = run(
            &self.options.pixi,
            &args,
            self.stage,
            self.options.timeout_seconds,
        )?;
        serde_json::from_str(&output)
            .map_err(|e| Error::Operation(format!("unreadable pixi search output: {e}")))
    }

    /// Releases the requirement excludes, plus failed-lookup notes for indexes
    /// that could not be consulted.
    fn excluded(
        &self,
        package: &str,
        requirement: &str,
        pypi: bool,
    ) -> Result<(Vec<Excluded>, Vec<String>)> {
        let cutoff = match &self.exclude_newer {
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
            .exclude_newer
            .as_ref()
            .map(|t| format!("exclude-newer {t}"));
        let label = |mut excluded: Vec<Excluded>| {
            for e in &mut excluded {
                e.policy = policy.clone();
            }
            excluded
        };
        if !pypi {
            let all = self.search(package)?;
            let allowed = self.search(&format!("{package} {requirement}"))?;
            return Ok((label(blocked_evidence(&all, &allowed, cutoff)), vec![]));
        }
        let mut pages = vec![];
        let mut failures = vec![];
        for index in &self.indexes {
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
}

/// Keep a constraint only when the manifest's conda channels or PyPI indexes
/// hold a newer release it excludes. Failed lookups stay as unestablished
/// suggestions, never as clean results.
fn suggest(
    constraints: Vec<Constraint>,
    target: &Target,
    availability: &Availability,
) -> Vec<Suggestion> {
    let mut suggestions = vec![];
    for constraint in constraints {
        let lookup = availability.excluded(
            &constraint.package,
            &constraint.requirement,
            constraint.pypi,
        );
        let unestablished = format!("Declared pin or upper bound can exclude newer releases. {REVIEW} Newer availability has not been established.");
        let (reason, evidence) = match lookup {
            Ok((excluded, failures)) if excluded.is_empty() && failures.is_empty() => continue,
            Ok((excluded, failures)) if excluded.is_empty() => (unestablished, failures),
            Ok((excluded, failures)) => (
                format!("Declared pin or upper bound excludes a newer release. {REVIEW}"),
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
            package: constraint.package,
            requirement: constraint.requirement,
            reason,
            evidence,
        });
    }
    suggestions
}
/// One rewritten declaration: (old requirement, new requirement, is PyPI).
type Edit = (String, String, bool);

/// Any version newer than a declared one; restyling it succeeds exactly when
/// the requirement's style is unambiguous.
const PROBE_VERSION: &str = "999999";

/// Apply `--accept` to the staged manifest. Every acceptance is checked before
/// any lookup, so ambiguous styles or malformed explicit requirements fail
/// without network access. Returns the edited text and validation notes.
fn accept_suggestions(
    content: &str,
    pyproject: bool,
    target: &Target,
    options: &UpdateOptions,
    availability: &Availability,
) -> Result<(String, Vec<String>)> {
    use crate::constraint::{parse_accept, restyle};
    use crate::pep440::{satisfies, Version};
    let acceptances = options
        .accept
        .iter()
        .map(|a| parse_accept(a))
        .collect::<Result<Vec<_>>>()?;
    for acceptance in &acceptances {
        let name = &acceptance.name;
        rewrite_requirements(content, pyproject, name, &mut |old, pypi| {
            match &acceptance.requirement {
                Some(requirement)
                    if pypi && satisfies(requirement, &Version::parse("0").unwrap()).is_none() =>
                {
                    Err(Error::Invalid(format!(
                        "{name}={requirement} is not a PEP 440 requirement; the first `=` separates the name, so pin exactly with --accept {name}===VERSION"
                    )))
                }
                Some(requirement) => Ok(requirement.clone()),
                None if restyle(old, PROBE_VERSION).is_some() => Ok(old.to_owned()),
                None => Err(Error::Invalid(format!(
                    "{name} {old}: the requirement style is ambiguous; pass --accept {name}=REQUIREMENT"
                ))),
            }
        })?;
    }
    let mut text = content.to_owned();
    let mut notes = vec![];
    for acceptance in &acceptances {
        let name = &acceptance.name;
        let mut cited = vec![];
        let (next, edits) = rewrite_requirements(&text, pyproject, name, &mut |old, pypi| {
            if let Some(requirement) = &acceptance.requirement {
                cited.push("explicit replacement".to_owned());
                return Ok(requirement.clone());
            }
            let (excluded, failures) = availability.excluded(name, old, pypi)?;
            let newest = excluded
                .iter()
                .max_by(|a, b| {
                    if pypi {
                        Version::parse(&a.version).cmp(&Version::parse(&b.version))
                    } else {
                        crate::conda_version::compare(&a.version, &b.version)
                    }
                })
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
            cited.push(newest.to_string());
            restyle(old, &newest.version).ok_or_else(|| {
                Error::Invalid(format!(
                    "{name} {old}: cannot restyle to {}; pass --accept {name}=REQUIREMENT",
                    newest.version
                ))
            })
        })?;
        text = next;
        for ((old, new, _), evidence) in edits.iter().zip(cited) {
            notes.push(format!(
                "{}: accepted {name} {old} -> {new} ({evidence})",
                target.id
            ));
        }
    }
    Ok((text, notes))
}

/// Replace a string value, keeping its surrounding comments/whitespace and its
/// literal (single-quoted) style when the new text allows it.
fn replace_string(value: &mut toml_edit::Value, new: &str) {
    let decor = value.decor().clone();
    let literal = match &*value {
        toml_edit::Value::String(formatted) => formatted
            .as_repr()
            .and_then(|r| r.as_raw().as_str())
            .is_some_and(|raw| raw.starts_with('\'')),
        _ => false,
    };
    *value = if literal && !new.contains(['\'', '\n']) {
        format!("'{new}'").parse().unwrap_or_else(|_| new.into())
    } else {
        new.into()
    };
    *value.decor_mut() = decor;
}

fn rewrite_tables(
    table: &mut dyn toml_edit::TableLike,
    name: &str,
    change: &mut dyn FnMut(&str, bool) -> Result<String>,
    edits: &mut Vec<Edit>,
    unversioned: &mut bool,
) -> Result<()> {
    for (key, item) in table.iter_mut() {
        let pypi = match key.get() {
            "dependencies" | "host-dependencies" | "build-dependencies" => Some(false),
            "pypi-dependencies" => Some(true),
            _ => None,
        };
        let Some(children) = item.as_table_like_mut() else {
            continue;
        };
        let Some(pypi) = pypi else {
            rewrite_tables(children, name, change, edits, unversioned)?;
            continue;
        };
        for (dependency, spec) in children.iter_mut() {
            let matches = if pypi {
                pypi_key(dependency.get()) == pypi_key(name)
            } else {
                dependency.get().eq_ignore_ascii_case(name)
            };
            if !matches {
                continue;
            }
            let value = if spec.is_str() {
                spec.as_value_mut()
            } else {
                spec.as_table_like_mut()
                    .and_then(|t| t.get_mut("version"))
                    .and_then(|v| v.as_value_mut())
                    .filter(|v| v.is_str())
            };
            let Some(value) = value else {
                *unversioned = true;
                continue;
            };
            let old = value.as_str().unwrap().to_owned();
            let new = change(&old, pypi)?;
            replace_string(value, &new);
            edits.push((old, new, pypi));
        }
    }
    Ok(())
}

/// Rewrite the version specifier of `name` in `[project]` and
/// `[dependency-groups]` requirement strings. Extras, markers, parentheses and
/// spacing around the specifier are kept; direct URLs count as unversioned.
fn rewrite_project(
    document: &mut toml_edit::DocumentMut,
    name: &str,
    change: &mut dyn FnMut(&str, bool) -> Result<String>,
    edits: &mut Vec<Edit>,
    unversioned: &mut bool,
) -> Result<()> {
    let mut arrays: Vec<&mut toml_edit::Array> = vec![];
    let (project, groups) = {
        let table = document.as_table_mut();
        let mut project = None;
        let mut groups = None;
        for (key, item) in table.iter_mut() {
            match key.get() {
                "project" => project = item.as_table_like_mut(),
                "dependency-groups" => groups = item.as_table_like_mut(),
                _ => {}
            }
        }
        (project, groups)
    };
    if let Some(project) = project {
        for (key, item) in project.iter_mut() {
            match key.get() {
                "dependencies" => arrays.extend(item.as_array_mut()),
                "optional-dependencies" => arrays.extend(
                    item.as_table_like_mut()
                        .into_iter()
                        .flat_map(|t| t.iter_mut().filter_map(|(_, v)| v.as_array_mut())),
                ),
                _ => {}
            }
        }
    }
    if let Some(groups) = groups {
        arrays.extend(groups.iter_mut().filter_map(|(_, v)| v.as_array_mut()));
    }
    for array in arrays {
        for value in array.iter_mut() {
            let Some(text) = value.as_str() else {
                continue;
            };
            let Some(requirement) = Pep508::parse(text) else {
                continue;
            };
            if pypi_key(&requirement.name) != pypi_key(name) {
                continue;
            }
            let Some(range) = requirement.specifier else {
                *unversioned = true;
                continue;
            };
            let old = text[range.clone()].to_owned();
            let new = change(&old, true)?;
            let spliced = format!("{}{new}{}", &text[..range.start], &text[range.end..]);
            replace_string(value, &spliced);
            edits.push((old, new, true));
        }
    }
    Ok(())
}

/// Rewrite every versioned declaration of `name` in the dependency tables,
/// keeping comments and layout. `change` receives the old requirement and
/// whether the declaration is a PyPI one. Returns (old, new, pypi) per edit.
fn rewrite_requirements(
    text: &str,
    pyproject: bool,
    name: &str,
    change: &mut dyn FnMut(&str, bool) -> Result<String>,
) -> Result<(String, Vec<Edit>)> {
    let mut document: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| Error::Invalid(format!("invalid manifest: {e}")))?;
    let root = if pyproject {
        document
            .get_mut("tool")
            .and_then(|t| t.get_mut("pixi"))
            .and_then(|p| p.as_table_like_mut())
    } else {
        Some(document.as_table_mut() as &mut dyn toml_edit::TableLike)
    };
    let mut edits = vec![];
    let mut unversioned = false;
    if let Some(root) = root {
        rewrite_tables(root, name, change, &mut edits, &mut unversioned)?;
    }
    if pyproject {
        rewrite_project(&mut document, name, change, &mut edits, &mut unversioned)?;
    }
    if edits.is_empty() {
        return Err(Error::Invalid(if unversioned {
            format!("{name} is declared with no version requirement to accept")
        } else {
            format!("{name} is not a declared dependency")
        }));
    }
    Ok((document.to_string(), edits))
}

fn validate_paths(value: &toml::Value, base: &Path, stage: &Path) -> Result<()> {
    // Compare canonical spellings on both sides (e.g. macOS /var -> /private/var).
    let stage = &crate::working_tree::canonical(stage)?;
    match value {
        toml::Value::Table(t) => {
            for (key, v) in t {
                if key == "path" {
                    if let Some(p) = v.as_str() {
                        let resolved =
                            crate::working_tree::canonical(&base.join(p)).map_err(|_| {
                                Error::Invalid(format!("local path unavailable in stage: {p}"))
                            })?;
                        if !resolved.starts_with(stage) {
                            return Err(Error::Invalid(format!(
                                "local dependency escapes the repository: {p}"
                            )));
                        }
                    }
                }
                validate_paths(v, base, stage)?;
            }
        }
        toml::Value::Array(a) => {
            for v in a {
                validate_paths(v, base, stage)?;
            }
        }
        toml::Value::String(s) if s.contains("file:") => {
            return Err(Error::Invalid("file: dependency URLs are not relocatable; use a repository-relative path declaration".into()));
        }
        toml::Value::String(s) => {
            if let Some((_, location)) = s.split_once(" @ ") {
                if location.starts_with('.')
                    || location.starts_with('/')
                    || location.as_bytes().get(1) == Some(&b':')
                {
                    let resolved =
                        crate::working_tree::canonical(&base.join(location)).map_err(|_| {
                            Error::Invalid("dependency reference is unavailable in stage".into())
                        })?;
                    if !resolved.starts_with(stage) {
                        return Err(Error::Invalid("dependency reference escapes stage".into()));
                    }
                }
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suggested(manifest: &str) -> Vec<String> {
        let mut output = vec![];
        collect_constraints(&manifest.parse().unwrap(), &mut output);
        output.into_iter().map(|c| c.package).collect()
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
        let evidence: Vec<String> = blocked_evidence(&all, &allowed, None)
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
        assert!(blocked_evidence(&allowed, &allowed, None).is_empty());
    }

    #[test]
    fn accepted_requirements_keep_comments_layout_and_ecosystem() {
        let manifest = r#"[dependencies]
ruff = "==0.15.22"      # pinned due to ruff syntax/rule updates
[feature.lint.dependencies]
ruff = { version = "==0.15.22", channel = "conda-forge" }
[pypi-dependencies]
Six = "==1.15.0" # keep
local = { path = "local", editable = true }
"#;
        let mut seen = vec![];
        let (text, edits) = rewrite_requirements(manifest, false, "RUFF", &mut |old, pypi| {
            seen.push(pypi);
            Ok(format!("{old}.1"))
        })
        .unwrap();
        assert_eq!(seen, [false, false]);
        assert_eq!(edits.len(), 2);
        assert_eq!(
            text,
            manifest
                .replace(r#"ruff = "==0.15.22"  "#, r#"ruff = "==0.15.22.1"  "#)
                .replace(r#"version = "==0.15.22""#, r#"version = "==0.15.22.1""#)
        );
        let (text, edits) =
            rewrite_requirements(manifest, false, "six", &mut |_, _| Ok("==1.17.0".into()))
                .unwrap();
        assert_eq!(edits, [("==1.15.0".into(), "==1.17.0".into(), true)]);
        assert!(text.contains("Six = \"==1.17.0\" # keep\n"), "{text}");
        let error =
            rewrite_requirements(manifest, false, "local", &mut |_, _| Ok("1".into())).unwrap_err();
        assert!(
            matches!(&error, Error::Invalid(m) if m.contains("no version")),
            "{error}"
        );
        let pyproject = r#"[project]
dependencies = [
  "Six[socks] (==1.15.0) ; python_version < '3.12'", # keep
  'urllib3>=1.26,<2',
  "direct @ https://example.invalid/direct-1.0.tar.gz",
]
[project.optional-dependencies]
extra = ["six==1.15.0"]
[dependency-groups]
dev = ["six ==1.15.0", { include-group = "extra" }]
"#;
        let (text, edits) = rewrite_requirements(pyproject, true, "six", &mut |old, pypi| {
            assert!(pypi);
            assert_eq!(old, "==1.15.0");
            Ok("==1.17.0".into())
        })
        .unwrap();
        assert_eq!(edits.len(), 3);
        assert_eq!(text, pyproject.replace("==1.15.0", "==1.17.0"));
        let (text, _) =
            rewrite_requirements(pyproject, true, "urllib3", &mut |_, _| Ok("<3".into())).unwrap();
        assert!(text.contains("'urllib3<3',"), "{text}");
        let error = rewrite_requirements(pyproject, true, "direct", &mut |_, _| Ok("1".into()))
            .unwrap_err();
        assert!(
            matches!(&error, Error::Invalid(m) if m.contains("no version")),
            "{error}"
        );
        let pyproject = "[tool.pixi.dependencies]\nruff = '==1' # c\n";
        let (text, _) =
            rewrite_requirements(pyproject, true, "ruff", &mut |_, _| Ok("==2".into())).unwrap();
        assert_eq!(text, "[tool.pixi.dependencies]\nruff = '==2' # c\n");
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
            blocked_evidence(&all, &allowed, cutoff)
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
        assert!(blocked_evidence(&with_rc, &allowed, None).is_empty());
        assert_eq!(versions(Some(2_500_000_000_000)), ["0.16.0"]);
        assert!(versions(Some(1_500_000_000_000)).is_empty());
    }

    /// macOS temp directories sit behind a symlink (/var -> /private/var), so the
    /// stage path and canonical dependency paths differ in spelling.
    #[cfg(unix)]
    #[test]
    fn local_paths_inside_a_symlinked_stage_are_accepted() {
        let real = tempfile::tempdir().unwrap();
        fs::create_dir(real.path().join("local")).unwrap();
        let aliases = tempfile::tempdir().unwrap();
        let alias = aliases.path().join("stage");
        std::os::unix::fs::symlink(real.path(), &alias).unwrap();
        let manifest: toml::Value = "[pypi-dependencies]\nlocal = { path = \"local\" }\n"
            .parse()
            .unwrap();
        validate_paths(&manifest, &alias, &alias).unwrap();
        let escape: toml::Value = "[pypi-dependencies]\nout = { path = \"..\" }\n"
            .parse()
            .unwrap();
        assert!(validate_paths(&escape, &alias, &alias).is_err());
    }

    #[test]
    fn pyproject_standard_requirements_are_pypi_constraints() {
        let manifest = r#"
[project]
dependencies = [
  "requests>=2,<3",
  "six==1.15.0; python_version < '3.12'",
  "urllib3[socks] (<2)",
  "floor>=1",
  "bare",
  "direct @ https://example.invalid/direct-1.0.tar.gz",
]
[project.optional-dependencies]
extra = ["compatible ~=1.4"]
[dependency-groups]
lint = ["ruff==0.15.22", { include-group = "extra" }]
[tool.pixi.dependencies]
python = "3.12.*"
"#;
        let parsed: toml::Value = manifest.parse().unwrap();
        let found: Vec<(String, String, bool)> = manifest_constraints(&parsed, true)
            .into_iter()
            .map(|c| (c.package, c.requirement, c.pypi))
            .collect();
        let expected = [
            ("python", "3.12.*", false),
            ("requests", ">=2,<3", true),
            ("six", "==1.15.0", true),
            ("urllib3", "<2", true),
            ("compatible", "~=1.4", true),
            ("ruff", "==0.15.22", true),
        ]
        .map(|(n, r, p)| (n.to_owned(), r.to_owned(), p));
        assert_eq!(found, expected);
    }

    #[test]
    fn lower_bounds_alone_never_block_newer_releases() {
        let manifest = r#"
[dependencies]
floor = ">=4.0.0"
strict = ">3"
excluded = "!=1.2"
any = "*"
ceiling = "<=12.9.0"
exact = "==5.1.0"
bare = "2.17"
prefix = "1.2.*"
conda-fuzzy = "=1.2"
range = ">=3.8,<4"
open-alternative = "1.0|>=2"
bounded-alternatives = "1.0|2.*"
[pypi-dependencies]
compatible = "~=1.4"
table-floor = { version = ">=3.8" }
table-ceiling = { version = "<2" }
"#;
        let mut names = suggested(manifest);
        names.sort();
        assert_eq!(
            names,
            [
                "bare",
                "bounded-alternatives",
                "ceiling",
                "compatible",
                "conda-fuzzy",
                "exact",
                "prefix",
                "range",
                "table-ceiling"
            ]
        );
    }
}

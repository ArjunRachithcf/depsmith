//! depsmith's engine: discover targets in a repository, prepare a reviewable
//! proposal by resolving each target inside a stage with its native package
//! manager, optionally scan it for vulnerabilities, and apply it exactly as
//! reviewed.
//!
//! The CLI and the Python API are thin layers over this crate. Terms follow
//! the project glossary (`CONTEXT.md`).
//!
//! ```no_run
//! use depsmith_core::{apply, discover, Engine, UpdateOptions};
//!
//! let repo = tempfile::tempdir()?;
//! std::fs::write(repo.path().join("pixi.toml"), "[workspace]\nname = 'demo'\n")?;
//! let targets: Vec<String> = discover(repo.path())?.into_iter().map(|t| t.id).collect();
//!
//! let options = UpdateOptions { pixi: "pixi".into(), ..Default::default() };
//! let proposal = Engine::default().prepare(repo.path(), &targets, options)?;
//! // Review proposal.changes, .dependencies and .suggestions, then write it:
//! if proposal.failures.is_empty() && !proposal.changes.is_empty() {
//!     apply(&proposal, false)?;
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
/// The adapter contract implemented by each package manager integration.
pub mod adapter;
mod conda_version;
mod constraint;
mod cutoff;
/// Reading resolved packages from lockfiles.
pub mod inventory;
mod model;
mod pep440;
/// The Pixi adapter: `pixi.toml` and Pixi-managed `pyproject.toml` targets.
pub mod pixi;
/// Running package managers and scanners: timeouts, cancellation of whole
/// process trees, and redacted diagnostics.
pub mod process;
mod pypi;
/// Vulnerability scanning of inventories.
pub mod scan;
mod scm;
mod working_tree;
use adapter::Adapter;
pub use model::*;
use std::{fs, path::Path};

/// Discovers targets and prepares proposals with a set of adapters. The
/// default engine has the Pixi and GitHub Actions adapters.
pub struct Engine {
    adapters: Vec<Box<dyn Adapter>>,
}
impl Default for Engine {
    fn default() -> Self {
        Self::new(vec![
            Box::new(pixi::Pixi),
            Box::new(actions::Actions::default()),
        ])
    }
}
/// Discover targets under `root` with the default adapters, skipping ignored,
/// environment and cache directories.
///
/// ```
/// let repo = tempfile::tempdir()?;
/// std::fs::write(repo.path().join("pixi.toml"), "[workspace]\nname = 'demo'\n")?;
/// let targets = depsmith_core::discover(repo.path())?;
/// assert_eq!(targets[0].id, "pixi:pixi.toml");
/// # Ok::<(), depsmith_core::Error>(())
/// ```
///
/// # Errors
///
/// Returns an error when `root` cannot be read.
pub fn discover(root: &Path) -> Result<Vec<Target>> {
    Engine::default().discover(root)
}
impl Engine {
    /// An engine with exactly these adapters.
    pub fn new(adapters: Vec<Box<dyn Adapter>>) -> Self {
        Self { adapters }
    }
    /// Declared capabilities of every adapter.
    pub fn capabilities(&self) -> Vec<adapter::Capabilities> {
        self.adapters.iter().map(|a| a.capabilities()).collect()
    }
    /// Unsupported requests fail; restrictive ones (cooldown) also fail when not
    /// applicable, since ignoring them would relax policy. Other inapplicable
    /// requests are returned as notes for the proposal's validation record.
    fn enforce_capabilities(
        &self,
        targets: &[Target],
        options: &UpdateOptions,
    ) -> Result<Vec<String>> {
        use adapter::Support;
        let mut notes = vec![];
        for target in targets {
            let capabilities = self.adapter(target).capabilities();
            let requested = [
                (
                    options.cooldown_days.is_some(),
                    "--cooldown-days",
                    capabilities.cooldown,
                    true,
                ),
                (
                    options.install,
                    "--install",
                    capabilities.install_validation,
                    false,
                ),
                (
                    options.refresh_git,
                    "--refresh-git",
                    capabilities.git_refresh,
                    false,
                ),
                (
                    options.upgrade,
                    "--upgrade",
                    capabilities.constraint_changes,
                    false,
                ),
                (
                    !options.packages.is_empty(),
                    "package selection",
                    capabilities.package_selection,
                    false,
                ),
                (
                    !options.accept.is_empty(),
                    "--accept",
                    capabilities.suggestion_acceptance,
                    false,
                ),
            ];
            for (asked, name, support, restrictive) in requested {
                match support {
                    _ if !asked => {}
                    Support::Supported => {}
                    Support::NotApplicable if !restrictive => notes.push(format!(
                        "{}: {name} not applicable to {}",
                        target.id, target.manager
                    )),
                    Support::NotApplicable => {
                        return Err(Error::Invalid(format!(
                            "{}: {name} cannot be enforced by the {} adapter",
                            target.id, target.manager
                        )))
                    }
                    Support::Unsupported(hint) => {
                        return Err(Error::Invalid(format!(
                            "{}: {name} is not supported by the {} adapter: {hint}",
                            target.id, target.manager
                        )))
                    }
                }
            }
        }
        Ok(notes)
    }
    fn adapter(&self, target: &Target) -> &dyn Adapter {
        self.adapters
            .iter()
            .find(|a| a.manager() == target.manager)
            .map(Box::as_ref)
            .expect("discovered targets always have a registered adapter")
    }
    /// Discover targets under `root` with this engine's adapters.
    ///
    /// # Errors
    ///
    /// Returns an error when `root` cannot be read.
    pub fn discover(&self, root: &Path) -> Result<Vec<Target>> {
        let root = working_tree::canonical(root)?;
        let mut targets = vec![];
        for path in working_tree::files(&root)? {
            if !matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("toml" | "yml" | "yaml")
            ) {
                continue;
            }
            let content = fs::read_to_string(root.join(&path))?;
            for adapter in &self.adapters {
                if adapter.detects(&path, &content) {
                    targets.push(Target {
                        id: format!(
                            "{}:{}",
                            adapter.manager(),
                            path.to_string_lossy().replace('\\', "/")
                        ),
                        manager: adapter.manager().into(),
                        manifest: path.clone(),
                    });
                }
            }
        }
        Ok(targets)
    }
    /// Prepare a proposal for the `selected` target identifiers under `root`.
    ///
    /// Options are validated and checked against each adapter's capabilities, and
    /// `--package`/`--accept` names against the declared direct dependencies,
    /// before any package manager runs. Each target is then resolved in its own
    /// stage; the repository is not modified. A target that cannot be prepared is
    /// recorded in [`Proposal::failures`] instead of failing the whole call.
    ///
    /// ```
    /// use depsmith_core::{Engine, UpdateOptions};
    ///
    /// let repo = tempfile::tempdir()?;
    /// std::fs::write(repo.path().join("pixi.toml"), "[workspace]\nname = 'demo'\n")?;
    /// let options = UpdateOptions { pixi: "no-such-pixi".into(), ..Default::default() };
    /// let proposal = Engine::default().prepare(repo.path(), &["pixi:pixi.toml".into()], options)?;
    /// // The missing package manager is a per-target failure, not an error.
    /// assert_eq!(proposal.failures.len(), 1);
    /// assert_eq!(proposal.exit_code(true), 3);
    /// # Ok::<(), depsmith_core::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] for invalid options, no or unknown targets,
    /// unsupported requested capabilities, or selected packages that are not
    /// direct dependencies of any selected target.
    pub fn prepare(
        &self,
        root: &Path,
        selected: &[String],
        options: UpdateOptions,
    ) -> Result<Proposal> {
        let root = working_tree::canonical(root)?;
        options.validate()?;
        let found = self.discover(&root)?;
        if selected.is_empty() {
            return Err(Error::Invalid(
                "select target IDs explicitly or configure targets".into(),
            ));
        }
        let mut targets = vec![];
        for id in selected {
            let target = found
                .iter()
                .find(|t| &t.id == id)
                .ok_or_else(|| Error::Invalid(format!("unknown target: {id}")))?;
            if !targets.contains(target) {
                targets.push(target.clone());
            }
        }
        let notes = self.enforce_capabilities(&targets, &options)?;
        // Resolve --package once, against the source manifests, so a typo fails
        // before any backend runs and each target only receives names it declares.
        let mut selections = vec![];
        let mut matched = std::collections::BTreeSet::new();
        for target in &targets {
            if options.packages.is_empty() {
                selections.push(None);
                continue;
            }
            let selected = self
                .adapter(target)
                .select(&root, target, &options.packages)?;
            matched.extend(selected.keys().cloned());
            let mut declared: Vec<_> = selected.into_values().collect();
            declared.dedup();
            selections.push(Some(declared));
        }
        // Accepted names resolve the same way; each target receives the
        // declared spelling, keeping any explicit requirement.
        let acceptances = options
            .accept
            .iter()
            .map(|a| constraint::parse_accept(a))
            .collect::<Result<Vec<_>>>()?;
        let accept_names: Vec<String> = acceptances.iter().map(|a| a.name.clone()).collect();
        let mut accepted = vec![];
        for target in &targets {
            if accept_names.is_empty() {
                accepted.push(vec![]);
                continue;
            }
            let selected = self.adapter(target).select(&root, target, &accept_names)?;
            matched.extend(selected.keys().cloned());
            accepted.push(
                acceptances
                    .iter()
                    .filter_map(|a| {
                        let declared = selected.get(&a.name)?;
                        Some(match &a.requirement {
                            Some(requirement) => format!("{declared}={requirement}"),
                            None => declared.clone(),
                        })
                    })
                    .collect::<Vec<_>>(),
            );
        }
        let unmatched: Vec<_> = options
            .packages
            .iter()
            .chain(&accept_names)
            .filter(|p| !matched.contains(*p))
            .map(String::as_str)
            .collect();
        if !unmatched.is_empty() {
            return Err(Error::Invalid(format!(
                "not a direct dependency of any selected target: {}",
                unmatched.join(", ")
            )));
        }
        let inputs = working_tree::fingerprint(&root)?;
        let mut proposal = Proposal {
            schema_version: 1,
            root: root.clone(),
            targets: targets.clone(),
            changes: vec![],
            dependencies: vec![],
            suggestions: vec![],
            unresolved: vec![],
            failures: vec![],
            validation: vec![],
            scans: vec![],
            inputs,
        };
        proposal.validation.extend(notes);
        for ((target, selection), accept) in targets.into_iter().zip(selections).zip(accepted) {
            let adapter = self.adapter(&target);
            let options = UpdateOptions {
                accept,
                ..options.clone()
            };
            let options = match selection {
                Some(packages) if packages.is_empty() => {
                    proposal.validation.push(format!(
                        "{}: skipped; none of the selected packages are declared here",
                        target.id
                    ));
                    continue;
                }
                Some(packages) => UpdateOptions {
                    packages,
                    ..options.clone()
                },
                None => options.clone(),
            };
            let stage = tempfile::tempdir()?;
            working_tree::stage(&root, stage.path(), &proposal.inputs)?;
            let mut conflict = false;
            let prepared = adapter
                .prepare(stage.path(), &target, &options)
                .and_then(|candidate| {
                    if options.scan {
                        let report = scan::scan_pair(
                            &target.id,
                            if candidate.baseline_available {
                                Some(&candidate.before)
                            } else {
                                None
                            },
                            &candidate.after,
                            &options,
                            stage.path(),
                        )?;
                        let passed = report.policy_passed;
                        proposal.scans.push(report);
                        if !passed {
                            return Err(Error::Policy("vulnerability threshold exceeded".into()));
                        }
                    }
                    let mut changes = vec![];
                    for path in candidate.files {
                        working_tree::output_path(stage.path(), &path)?;
                        working_tree::output_path(&root, &path)?;
                        let after = fs::read_to_string(stage.path().join(&path))?;
                        let before = if root.join(&path).exists() {
                            Some(fs::read_to_string(root.join(&path))?)
                        } else {
                            None
                        };
                        if before.as_deref() != Some(&after) {
                            let diff = similar::TextDiff::from_lines(
                                before.as_deref().unwrap_or(""),
                                &after,
                            )
                            .unified_diff()
                            .header(
                                &format!("a/{}", path.display()),
                                &format!("b/{}", path.display()),
                            )
                            .to_string();
                            changes.push(FileChange {
                                target: target.id.clone(),
                                path,
                                before,
                                after,
                                diff,
                            });
                        }
                    }
                    if changes
                        .iter()
                        .any(|c| proposal.changes.iter().any(|p| p.path == c.path))
                    {
                        conflict = true;
                        return Err(Error::Invalid(
                            "targets propose overlapping file changes; select one owner".into(),
                        ));
                    }
                    for before in &candidate.before {
                        if !candidate.after.contains(before) {
                            let after = candidate
                                .after
                                .iter()
                                .find(|p| {
                                    p.name == before.name
                                        && p.ecosystem == before.ecosystem
                                        && p.platform == before.platform
                                })
                                .cloned();
                            proposal.dependencies.push(DependencyChange {
                                before: Some(before.clone()),
                                after,
                            });
                        }
                    }
                    for after in &candidate.after {
                        if !candidate.before.iter().any(|p| {
                            p.name == after.name
                                && p.ecosystem == after.ecosystem
                                && p.platform == after.platform
                        }) {
                            proposal.dependencies.push(DependencyChange {
                                before: None,
                                after: Some(after.clone()),
                            });
                        }
                    }
                    proposal.changes.extend(changes);
                    proposal.suggestions.extend(candidate.suggestions);
                    proposal.unresolved.extend(candidate.unresolved);
                    proposal.validation.extend(candidate.validation);
                    Ok(())
                });
            if let Err(error) = prepared {
                if conflict {
                    return Err(error);
                }
                proposal.failures.push(Failure {
                    target: target.id,
                    code: error.exit_code(),
                    message: error.to_string(),
                });
            }
        }
        Ok(proposal)
    }
}
mod transaction;
pub use transaction::{apply, recover};

/// The GitHub Actions adapter: remote `uses:` references in workflow files.
pub mod actions;
/// Repository configuration in `depsmith.toml`.
pub mod config;

/// Report adapter capabilities and whether each native tool is available and
/// a tested version, for troubleshooting an installation.
pub fn doctor(options: &UpdateOptions) -> serde_json::Value {
    let capabilities = Engine::default().capabilities();
    let mut tools = vec![];
    for tool in capabilities.iter().filter_map(|c| c.native_tool.as_ref()) {
        let program = match tool.option {
            "pixi" => &options.pixi,
            other => {
                tools.push(serde_json::json!({"tool": other, "available": false,
                    "status": "unavailable", "error": "no option configures this executable",
                    "tested_versions": tool.tested_versions}));
                continue;
            }
        };
        tools.push(tool_report(
            tool.option,
            program,
            tool.tested_versions,
            options,
        ));
    }
    tools.push(tool_report(
        "grype",
        &options.grype,
        scan::GRYPE_TESTED_VERSIONS,
        options,
    ));
    serde_json::json!({"schema_version": 1, "tools": tools, "adapters": capabilities})
}

fn tool_report(
    name: &str,
    program: &str,
    tested: &[&str],
    options: &UpdateOptions,
) -> serde_json::Value {
    let result = process::run(
        program,
        &["--version".into()],
        Path::new("."),
        options.timeout_seconds,
    );
    match result {
        Ok(output) => {
            let (status, version) = adapter::tool_status(&output, tested);
            serde_json::json!({"tool": name, "program": program, "available": true,
                "status": status, "version": version, "tested_versions": tested})
        }
        Err(error) => serde_json::json!({"tool": name, "program": program, "available": false,
            "status": "unavailable", "error": error.to_string(), "tested_versions": tested}),
    }
}

/// Scan the current locks of the `selected` targets under `root` without
/// preparing an update. There is no baseline to compare against, so every
/// report is `candidate-only`.
///
/// # Errors
///
/// Returns [`Error::Invalid`] for invalid options, no or unknown targets, or a
/// target without a lock to scan, and scanner errors as described for
/// [`scan::scan_pair`].
pub fn scan_existing(
    root: &Path,
    selected: &[String],
    options: &UpdateOptions,
) -> Result<Vec<scan::ScanReport>> {
    options.validate()?;
    if selected.is_empty() {
        return Err(Error::Invalid("select targets for scanning".into()));
    }
    let targets = discover(root)?;
    let mut reports = vec![];
    for id in selected {
        let target = targets
            .iter()
            .find(|t| &t.id == id)
            .ok_or_else(|| Error::Invalid(format!("unknown target: {id}")))?;
        let engine = Engine::default();
        let adapter = engine
            .adapters
            .iter()
            .find(|a| a.manager() == target.manager)
            .unwrap();
        let packages = adapter.inventory(root, target)?;
        reports.push(scan::scan_pair(id, None, &packages, options, root)?);
    }
    Ok(reports)
}

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
/// The Cargo adapter: `Cargo.lock` owners (workspaces and standalone crates).
pub mod cargo;
/// The conda adapter: `environment.yml` targets locked with conda-lock.
pub mod conda;
mod conda_version;
pub mod conformance;
mod constraint;
pub mod constraints;
mod cutoff;
mod ecosystem;
mod http;
/// Reading resolved packages from lockfiles.
pub mod inventory;
mod model;
mod pep440;
/// The Pixi adapter: `pixi.toml` and Pixi-managed `pyproject.toml` targets.
pub mod pixi;
/// Running package managers and scanners: timeouts, cancellation of whole
/// process trees, and redacted diagnostics.
pub mod process;
/// Downloading missing native tools, with consent, into a checksum-verified cache.
pub mod provision;
mod pypi;
mod pyproject;
/// Vulnerability scanning of inventories.
pub mod scan;
mod scm;
/// The uv adapter: `pyproject.toml` projects locked with `uv.lock`.
pub mod uv;
mod working_tree;
use adapter::Adapter;
pub use model::*;
use std::{fs, path::Path};

/// Discovers targets and prepares proposals with a set of adapters. The
/// default engine has the Pixi, GitHub Actions, Cargo, conda and uv adapters.
pub struct Engine {
    adapters: Vec<(Box<dyn Adapter>, adapter::AdapterSpec)>,
}
impl Default for Engine {
    fn default() -> Self {
        Self::new(vec![
            Box::new(pixi::Pixi),
            Box::new(actions::Actions::default()),
            Box::new(cargo::Cargo::default()),
            Box::new(conda::Conda),
            Box::new(uv::Uv::default()),
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
        Self {
            adapters: adapters
                .into_iter()
                .map(|adapter| {
                    let spec = adapter.spec();
                    (adapter, spec)
                })
                .collect(),
        }
    }
    /// Every adapter's spec: discovery, staging, tools and capabilities.
    pub fn specs(&self) -> Vec<adapter::AdapterSpec> {
        self.adapters.iter().map(|(_, spec)| spec.clone()).collect()
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
            let capabilities = self.spec(target).capabilities.clone();
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
            .find(|(_, spec)| spec.manager == target.manager)
            .map(|(adapter, _)| adapter.as_ref())
            .expect("discovered targets always have a registered adapter")
    }
    fn spec(&self, target: &Target) -> &adapter::AdapterSpec {
        self.adapters
            .iter()
            .find(|(_, spec)| spec.manager == target.manager)
            .map(|(_, spec)| spec)
            .expect("discovered targets always have a registered adapter")
    }
    fn layout(&self) -> working_tree::Layout {
        working_tree::Layout::new(self.adapters.iter().map(|(_, spec)| spec))
    }
    /// Discover targets under `root` with this engine's adapters.
    ///
    /// # Errors
    ///
    /// Returns an error when `root` cannot be read.
    pub fn discover(&self, root: &Path) -> Result<Vec<Target>> {
        let root = working_tree::canonical(root)?;
        let mut targets = vec![];
        for path in working_tree::files(&root, &self.layout())? {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let mut content = None;
            for (adapter, spec) in &self.adapters {
                if !spec
                    .patterns
                    .iter()
                    .any(|p| working_tree::matches_pattern(p, name))
                {
                    continue;
                }
                // Read each candidate once; files that are not UTF-8 text are
                // not manifests of any supported package manager.
                let text = content.get_or_insert_with(|| {
                    fs::read(root.join(&path))
                        .map(|bytes| String::from_utf8(bytes).ok())
                        .map_err(Error::from)
                });
                let text = match text {
                    Ok(Some(text)) => text,
                    Ok(None) => break,
                    Err(_) => {
                        return Err(Error::Operation(format!("cannot read {}", path.display())))
                    }
                };
                if adapter.detects_in(&root, &path, text) {
                    targets.push(Target {
                        id: format!(
                            "{}:{}",
                            spec.manager,
                            path.to_string_lossy().replace('\\', "/")
                        ),
                        manager: spec.manager.clone(),
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
        mut options: UpdateOptions,
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
        let mut notes = self.enforce_capabilities(&targets, &options)?;
        notes.extend(self.use_cached_tools(&root, &targets, &mut options));
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
        let layout = self.layout();
        let inputs = working_tree::fingerprint(&root, &layout)?;
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
            layout,
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
            working_tree::stage(&root, stage.path(), &proposal.inputs, &proposal.layout)?;
            let mut conflict = false;
            let prepared = adapter
                .availability(stage.path(), &target)
                .and_then(|config| {
                    // Acceptance edits the staged declarations before the
                    // adapter resolves; suggestions describe the result.
                    let lookup = constraints::Lookup::new(stage.path(), &options, config);
                    let accepted = constraints::accept(
                        adapter,
                        stage.path(),
                        &target,
                        &options.accept,
                        &lookup,
                    )?;
                    let resolve = UpdateOptions {
                        accept: vec![],
                        ..options.clone()
                    };
                    let mut candidate = adapter.prepare(stage.path(), &target, &resolve)?;
                    candidate.validation.splice(0..0, accepted);
                    candidate.suggestions.extend(constraints::suggest(
                        adapter,
                        stage.path(),
                        &target,
                        &lookup,
                    )?);
                    Ok(candidate)
                })
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
/// a tested version, for troubleshooting an installation. Uses the default
/// adapters; see [`Engine::doctor`].
pub fn doctor(root: &Path, options: &UpdateOptions) -> serde_json::Value {
    Engine::default().doctor(root, options)
}

impl Engine {
    /// Report this engine's adapter specs and, for every native tool they or
    /// the scanner declare, whether it is available and a tested version.
    /// Tool paths come from [`UpdateOptions::tool`], then `PATH`, then the
    /// installs `depsmith init` recorded for the repository at `root`.
    pub fn doctor(&self, root: &Path, options: &UpdateOptions) -> serde_json::Value {
        let specs = self.specs();
        let mut seen = std::collections::BTreeSet::new();
        let tools: Vec<_> = specs
            .iter()
            .flat_map(|spec| spec.tools.iter().cloned())
            .chain([scan::scanner_tool()])
            .filter(|tool| seen.insert(tool.name.clone()))
            .map(|tool| tool_report(&tool, options, root))
            .collect();
        serde_json::json!({"schema_version": 1, "tools": tools, "adapters": specs})
    }

    /// The native tools `targets` need (and the scanner when `options.scan`),
    /// once each, in adapter order.
    fn needed_tools(&self, targets: &[Target], options: &UpdateOptions) -> Vec<adapter::ToolSpec> {
        let mut seen = std::collections::BTreeSet::new();
        targets
            .iter()
            .flat_map(|t| self.spec(t).tools.clone())
            .chain(options.scan.then(scan::scanner_tool))
            .filter(|tool| seen.insert(tool.name.clone()))
            .collect()
    }

    /// The tools `targets` under `root` need (and the scanner when
    /// `options.scan`) that `depsmith init` should install: neither
    /// configured nor on `PATH`, and not a verified install recorded for
    /// `root` (absent, or changed, outdated or unrecorded in the tool cache).
    pub fn missing_tools(
        &self,
        root: &Path,
        targets: &[Target],
        options: &UpdateOptions,
    ) -> Vec<adapter::ToolSpec> {
        self.needed_tools(targets, options)
            .into_iter()
            .filter(|tool| {
                matches!(
                    locate(tool, options, root).1,
                    ToolSource::Missing | ToolSource::Untrusted(_)
                )
            })
            .collect()
    }

    /// Point `options` at the verified installs recorded for `root` of the
    /// tools `targets` need; returns a note for each install that is no
    /// longer trusted, which is not used.
    fn use_cached_tools(
        &self,
        root: &Path,
        targets: &[Target],
        options: &mut UpdateOptions,
    ) -> Vec<String> {
        let mut notes = vec![];
        for tool in self.needed_tools(targets, options) {
            match locate(&tool, options, root) {
                (program, ToolSource::Downloaded) => {
                    options.tools.insert(tool.name.clone(), program);
                }
                (_, ToolSource::Untrusted(reason)) => notes.push(format!(
                    "{reason}; not used: run `depsmith init` to install {} again",
                    tool.name
                )),
                _ => {}
            }
        }
        notes
    }

    /// Check the native tools the targets under `root` use (and the scanner
    /// when `options.scan`), and reinstall each one that has a pinned
    /// download for this host and that `consent` accepts, when it is
    /// missing, no longer the recorded install, or fails to run. Nothing is
    /// downloaded without consent; installs are recorded (with their
    /// sha256) in the repository's `.depsmith` directory, which ignores
    /// itself in Git.
    ///
    /// Returns a report with the discovered `targets`, a `doctor`-style
    /// entry per used tool (with the targets using it), the tools
    /// `installed`, and the tools still `missing` (unavailable).
    ///
    /// # Errors
    ///
    /// Returns an error when `root` cannot be read, there is no tool cache
    /// directory, or an accepted install fails (see [`provision::install`]).
    pub fn init(
        &self,
        root: &Path,
        options: &UpdateOptions,
        consent: &mut dyn FnMut(&adapter::ToolSpec) -> bool,
    ) -> Result<serde_json::Value> {
        options.validate()?;
        let targets = self.discover(root)?;
        let mut installed = vec![];
        for tool in self.needed_tools(&targets, options) {
            let needs_install = match locate(&tool, options, root).1 {
                ToolSource::Missing | ToolSource::Untrusted(_) => true,
                // A recorded install that no longer runs is offered again.
                ToolSource::Downloaded => tool_report(&tool, options, root)["available"] != true,
                ToolSource::Configured | ToolSource::Path => false,
            };
            if needs_install && provision::host_download(&tool).is_some() && consent(&tool) {
                let cache = provision::cache_dir().ok_or_else(|| {
                    Error::Invalid("no tool cache directory; set DEPSMITH_TOOLS_DIR".into())
                })?;
                provision::reinstall(root, &tool, &cache, options.timeout_seconds)?;
                installed.push(tool.name.clone());
            }
        }
        let mut missing = vec![];
        let tools: Vec<_> = self
            .needed_tools(&targets, options)
            .into_iter()
            .map(|tool| {
                let mut report = tool_report(&tool, options, root);
                if report["available"] != true {
                    missing.push(tool.name.clone());
                }
                let used_by: Vec<&str> = targets
                    .iter()
                    .filter(|t| self.spec(t).tools.iter().any(|u| u.name == tool.name))
                    .map(|t| t.id.as_str())
                    .collect();
                report["used_by"] = serde_json::json!(used_by);
                report
            })
            .collect();
        let ids: Vec<&str> = targets.iter().map(|t| t.id.as_str()).collect();
        Ok(
            serde_json::json!({"schema_version": 1, "targets": ids, "tools": tools,
            "installed": installed, "missing": missing}),
        )
    }

    /// Scan the current locks of the `selected` targets under `root` without
    /// preparing an update; see [`scan_existing`].
    ///
    /// # Errors
    ///
    /// As for [`scan_existing`].
    pub fn scan_existing(
        &self,
        root: &Path,
        selected: &[String],
        options: &UpdateOptions,
    ) -> Result<Vec<scan::ScanReport>> {
        options.validate()?;
        if selected.is_empty() {
            return Err(Error::Invalid("select targets for scanning".into()));
        }
        let targets = self.discover(root)?;
        let mut inventories = vec![];
        for id in selected {
            let target = targets
                .iter()
                .find(|t| &t.id == id)
                .ok_or_else(|| Error::Invalid(format!("unknown target: {id}")))?;
            inventories.push((id, self.adapter(target).inventory(root, target)?));
        }
        let mut options = options.clone();
        options.scan = true;
        self.use_cached_tools(root, &[], &mut options);
        let mut reports = vec![];
        for (id, packages) in inventories {
            reports.push(scan::scan_pair(id, None, &packages, &options, root)?);
        }
        Ok(reports)
    }
}

fn tool_report(
    tool: &adapter::ToolSpec,
    options: &UpdateOptions,
    root: &Path,
) -> serde_json::Value {
    let (program, source) = locate(tool, options, root);
    let downloadable = provision::host_download(tool).is_some();
    let untrusted = match &source {
        ToolSource::Untrusted(reason) => Some(reason.clone()),
        _ => None,
    };
    let name = &tool.name;
    let tested = &tool.tested_versions;
    let tested_refs: Vec<&str> = tested.iter().map(String::as_str).collect();
    let result = process::run(
        &program,
        &["--version".into()],
        Path::new("."),
        options.timeout_seconds,
    );
    match result {
        Ok(output) => {
            let (status, version) = adapter::tool_status(&output, &tested_refs);
            serde_json::json!({"tool": name, "program": program, "available": true,
                "status": status, "version": version, "tested_versions": tested,
                "source": source.label(), "downloadable": downloadable, "untrusted": untrusted})
        }
        Err(error) => serde_json::json!({"tool": name, "program": program, "available": false,
            "status": "unavailable", "error": error.to_string(), "tested_versions": tested,
            "source": source.label(), "downloadable": downloadable, "untrusted": untrusted}),
    }
}

/// Whether `program` names an executable file: a path, or a name found on
/// `PATH` (with `PATHEXT` extensions on Windows).
fn on_path(program: &str) -> bool {
    let path = Path::new(program);
    if path.components().count() > 1 {
        return path.is_file();
    }
    let extensions: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".into())
            .split(';')
            .map(str::to_owned)
            .chain([String::new()])
            .collect()
    } else {
        vec![String::new()]
    };
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            extensions
                .iter()
                .any(|e| dir.join(format!("{program}{e}")).is_file())
        })
    })
}

/// Where the executable run for a tool comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ToolSource {
    /// A tool path option names it.
    Configured,
    /// The default executable is on `PATH`.
    Path,
    /// The verified install `depsmith init` recorded for the repository.
    Downloaded,
    /// A tool cache entry the repository cannot trust, and why.
    Untrusted(String),
    /// Nowhere: the default executable is absent.
    Missing,
}

impl ToolSource {
    fn label(&self) -> &'static str {
        match self {
            Self::Configured => "configured",
            Self::Path => "path",
            Self::Downloaded => "downloaded",
            Self::Untrusted(_) => "untrusted",
            Self::Missing => "missing",
        }
    }
}

/// The executable to run for `tool` in the repository at `root` and where
/// it comes from. A configured path is used as given, even when it does not
/// exist: the user chose it. A tool cache entry is used only while it is
/// the unchanged install recorded for `root`.
fn locate(tool: &adapter::ToolSpec, options: &UpdateOptions, root: &Path) -> (String, ToolSource) {
    let program = options.tool(&tool.name);
    if program != tool.default {
        return (program, ToolSource::Configured);
    }
    if on_path(&program) {
        return (program, ToolSource::Path);
    }
    let Some(cache) = provision::cache_dir() else {
        return (program, ToolSource::Missing);
    };
    match provision::trust(root, tool, &cache) {
        provision::Trust::Verified(path) => {
            (path.to_string_lossy().into_owned(), ToolSource::Downloaded)
        }
        provision::Trust::Untrusted(reason) => (program, ToolSource::Untrusted(reason)),
        provision::Trust::Unrecorded => (
            program,
            ToolSource::Untrusted(format!(
                "{} is in the tool cache but was not installed by `depsmith init` for this repository",
                tool.name
            )),
        ),
        provision::Trust::Absent => (program, ToolSource::Missing),
    }
}

/// Check the native tools the targets under `root` use and install the
/// missing ones `consent` accepts, with the default adapters; see
/// [`Engine::init`].
///
/// # Errors
///
/// As for [`Engine::init`].
pub fn init(
    root: &Path,
    options: &UpdateOptions,
    consent: &mut dyn FnMut(&adapter::ToolSpec) -> bool,
) -> Result<serde_json::Value> {
    Engine::default().init(root, options, consent)
}

/// Scan the current locks of the `selected` targets under `root` without
/// preparing an update, with the default adapters. There is no baseline to
/// compare against, so every report is `candidate-only`.
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
    Engine::default().scan_existing(root, selected, options)
}

//! The adapter contract: how a package manager integration declares its
//! capabilities, selects direct dependencies, and prepares a candidate inside
//! a stage for the engine to review and apply.
use crate::{Package, Result, Suggestion, Target, Unresolved, UpdateOptions};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// PEP 503 name normalization, for PyPI names only; conda distinguishes `-`/`_`.
pub fn pypi_key(name: &str) -> String {
    let mut key = String::with_capacity(name.len());
    for c in name.trim().chars() {
        if matches!(c, '-' | '_' | '.') {
            if !key.ends_with('-') {
                key.push('-');
            }
        } else {
            key.extend(c.to_lowercase());
        }
    }
    key
}

/// Whether an adapter honours a requested behaviour. `Unsupported` carries an
/// actionable hint; `NotApplicable` means the ecosystem has no such concept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "status", content = "hint", rename_all = "kebab-case")]
pub enum Support {
    /// The adapter honours the behaviour.
    Supported,
    /// The adapter cannot honour it; the hint says what to do instead.
    Unsupported(&'static str),
    /// The ecosystem has no such concept.
    NotApplicable,
}

/// The native executable an adapter drives. Only versions actually exercised
/// are listed; others are reported as untested, not assumed incompatible.
#[derive(Debug, Clone, Serialize)]
pub struct NativeTool {
    /// `UpdateOptions` field naming the executable.
    pub option: &'static str,
    /// Versions exercised by depsmith's tests.
    pub tested_versions: &'static [&'static str],
}

/// What an adapter supports, reported by `doctor` and enforced by the engine
/// before any work: an unsupported request fails instead of being ignored.
#[derive(Debug, Clone, Serialize)]
pub struct Capabilities {
    /// The package manager the adapter integrates.
    pub manager: &'static str,
    /// Updating selected direct dependencies (`--package`).
    pub package_selection: Support,
    /// Upgrading with changed constraints (`--upgrade`).
    pub constraint_changes: Support,
    /// Rewriting a declared requirement to accept a suggestion (`--accept`).
    pub suggestion_acceptance: Support,
    /// Moving Git pins to newer commits (`--refresh-git`).
    pub git_refresh: Support,
    /// Enforcing a common minimum release age (`--cooldown-days`).
    pub cooldown: Support,
    /// Installing the candidate as a validation level (`--install`).
    pub install_validation: Support,
    /// Producing a lockfile with exact resolutions.
    pub lockfile: Support,
    /// Resolving for every configured platform.
    pub platforms: Support,
    /// The native executable the adapter drives, if any.
    pub native_tool: Option<NativeTool>,
}

impl Capabilities {
    /// Nothing is assumed: every behaviour must be declared to be requested.
    pub fn undeclared(manager: &'static str) -> Self {
        let no = Support::Unsupported("not declared by this adapter");
        Self {
            manager,
            package_selection: no,
            constraint_changes: no,
            suggestion_acceptance: no,
            git_refresh: no,
            cooldown: no,
            install_validation: no,
            lockfile: no,
            platforms: no,
            native_tool: None,
        }
    }
}

/// Classify `tool --version` output (`name X.Y.Z`) against tested versions.
pub fn tool_status(output: &str, tested: &[&str]) -> (&'static str, Option<String>) {
    let version = output
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().last())
        .filter(|token| token.starts_with(|c: char| c.is_ascii_digit()))
        .map(str::to_owned);
    match &version {
        Some(v) if tested.contains(&v.as_str()) => ("tested", version),
        _ => ("untested", version),
    }
}

/// The proposed state of one target after resolution in the stage.
#[derive(Default)]
pub struct Candidate {
    /// Files the adapter produced, relative to the stage; the engine diffs them
    /// against the repository to build file changes.
    pub files: Vec<PathBuf>,
    /// Suggestions about the target's declared constraints.
    pub suggestions: Vec<Suggestion>,
    /// References left unchanged because they could not be resolved.
    pub unresolved: Vec<Unresolved>,
    /// Baseline inventory, empty when there was no lock.
    pub before: Vec<Package>,
    /// Whether a baseline lock existed.
    pub baseline_available: bool,
    /// Candidate inventory.
    pub after: Vec<Package>,
    /// Validation levels completed and notes such as accepted suggestions.
    pub validation: Vec<String>,
}

/// Adapters only prepare inside a disposable stage. All source writes belong to the engine.
pub trait Adapter: Send + Sync {
    /// Name of the package manager, used as the prefix of target identifiers.
    fn manager(&self) -> &'static str;
    /// Whether the file at `relative` (with `content`) is a target of this adapter.
    fn detects(&self, relative: &Path, content: &str) -> bool;
    /// Declared capabilities; the default declares nothing.
    fn capabilities(&self) -> Capabilities {
        Capabilities::undeclared(self.manager())
    }
    /// The resolved packages of `target` under `root`, for scanning an existing
    /// lock without preparing an update.
    fn inventory(&self, _root: &Path, _target: &Target) -> Result<Vec<Package>> {
        Err(crate::Error::Invalid(
            "inventory not supported by this adapter".into(),
        ))
    }
    /// Match requested `--package` names against direct dependencies declared in
    /// the source root (read-only). Returns requested name -> declared name for
    /// the requests this target owns; unmatched requests are omitted.
    fn select(
        &self,
        _root: &Path,
        _target: &Target,
        _requested: &[String],
    ) -> Result<BTreeMap<String, String>> {
        Err(crate::Error::Invalid(format!(
            "package selection is not supported by the {} adapter",
            self.manager()
        )))
    }
    /// Resolve `target` inside `stage`, a disposable copy of the repository, and
    /// return the candidate. Must not write outside `stage`.
    fn prepare(&self, stage: &Path, target: &Target, options: &UpdateOptions) -> Result<Candidate>;
}

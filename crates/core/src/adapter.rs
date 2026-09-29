//! The adapter contract: how a package manager integration declares its
//! capabilities, selects direct dependencies, and prepares a candidate inside
//! a stage for the engine to review and apply.
use crate::{
    constraints::{AvailabilityConfig, Declaration, Edit},
    Package, Result, Suggestion, Target, Unresolved, UpdateOptions,
};
use serde::{Deserialize, Serialize};
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "hint", rename_all = "kebab-case")]
pub enum Support {
    /// The adapter honours the behaviour.
    Supported,
    /// The adapter cannot honour it; the hint says what to do instead.
    Unsupported(String),
    /// The ecosystem has no such concept.
    NotApplicable,
}

/// A native executable an adapter drives. Only versions actually exercised
/// are listed; others are reported as untested, not assumed incompatible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Name used in `UpdateOptions::tools` and `--tool NAME=PATH`.
    pub name: String,
    /// Executable run when no path is configured.
    pub default: String,
    /// Versions exercised by depsmith's tests.
    pub tested_versions: Vec<String>,
}

/// Files that are resolver inputs of a target even when they are ignored by
/// Git, such as a lockfile or native configuration next to the manifest. They
/// are staged and fingerprinted like any other input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedFiles {
    /// File-name patterns of the manifests this rule applies to.
    pub manifests: Vec<String>,
    /// Paths relative to the manifest's directory.
    pub inputs: Vec<String>,
}

/// Everything the engine needs to know about an adapter, as plain data: how
/// its targets are found and staged, which native tools it drives and which
/// behaviours it supports. Adding a package manager needs no engine changes
/// beyond registering its adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterSpec {
    /// Name of the package manager; the prefix of its target identifiers.
    pub manager: String,
    /// Ecosystems whose packages the manager resolves.
    pub ecosystems: Vec<String>,
    /// File-name patterns (`*` matches any run of characters) of candidate
    /// manifests; only matching files are offered to [`Adapter::detects`].
    pub patterns: Vec<String>,
    /// Ignored files that are still inputs of a target.
    pub managed: Vec<ManagedFiles>,
    /// Directory names holding installed environments or caches, never walked.
    pub skip_dirs: Vec<String>,
    /// Native executables the adapter drives.
    pub tools: Vec<ToolSpec>,
    /// Behaviours the adapter supports.
    #[serde(flatten)]
    pub capabilities: Capabilities,
}

impl AdapterSpec {
    /// A spec with only a manager name and discovery patterns: no managed
    /// files, skipped directories, tools, ecosystems or capabilities.
    pub fn new(manager: &str, patterns: &[&str]) -> Self {
        Self {
            manager: manager.into(),
            ecosystems: vec![],
            patterns: patterns.iter().map(|p| p.to_string()).collect(),
            managed: vec![],
            skip_dirs: vec![],
            tools: vec![],
            capabilities: Capabilities::undeclared(),
        }
    }
}

/// What an adapter supports, reported by `doctor` and enforced by the engine
/// before any work: an unsupported request fails instead of being ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
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
}

impl Capabilities {
    /// Nothing is assumed: every behaviour must be declared to be requested.
    pub fn undeclared() -> Self {
        let no = Support::Unsupported("not declared by this adapter".into());
        Self {
            package_selection: no.clone(),
            constraint_changes: no.clone(),
            suggestion_acceptance: no.clone(),
            git_refresh: no.clone(),
            cooldown: no.clone(),
            install_validation: no.clone(),
            lockfile: no.clone(),
            platforms: no,
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    /// The adapter's spec: discovery, staging, tools and capabilities.
    fn spec(&self) -> AdapterSpec;
    /// Name of the package manager, from [`Adapter::spec`].
    fn manager(&self) -> String {
        self.spec().manager
    }
    /// Whether the file at `relative` (with `content`) is a target of this
    /// adapter. Only files matching the spec's patterns are offered.
    fn detects(&self, relative: &Path, content: &str) -> bool;
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
    /// Where the target declares its direct dependencies and their
    /// constraints, read from `root` (the repository or a stage). The engine
    /// derives suggestions and `--accept` edits from these. The default
    /// declares nothing.
    fn declarations(&self, _root: &Path, _target: &Target) -> Result<Vec<Declaration>> {
        Ok(vec![])
    }
    /// Apply accepted requirements to the target's files in `stage`, keeping
    /// everything else (comments, layout, other declarations) unchanged.
    fn rewrite(&self, _stage: &Path, _target: &Target, _edits: &[Edit]) -> Result<()> {
        Err(crate::Error::Invalid(format!(
            "the {} adapter cannot rewrite declarations",
            self.manager()
        )))
    }
    /// Where the target's packages are published and which release-age policy
    /// applies, for availability evidence. The default consults no registry.
    fn availability(&self, _root: &Path, _target: &Target) -> Result<AvailabilityConfig> {
        Ok(AvailabilityConfig::default())
    }
    /// Resolve `target` inside `stage`, a disposable copy of the repository, and
    /// return the candidate. Must not write outside `stage`.
    fn prepare(&self, stage: &Path, target: &Target, options: &UpdateOptions) -> Result<Candidate>;
}

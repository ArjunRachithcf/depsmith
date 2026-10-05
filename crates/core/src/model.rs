//! Data types shared by the engine, the CLI and the Python bindings: targets,
//! update options, proposals and their parts, and the error type.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

/// Why an operation did not succeed. Each variant maps to a CLI exit status
/// (see [`Error::exit_code`]) and to a Python exception type.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The request itself is wrong: bad options or configuration, unknown or
    /// ambiguous targets, or a precondition the repository does not meet.
    #[error("invalid request: {0}")]
    Invalid(String),
    /// A package manager, scanner or other operation failed while running.
    #[error("operation failed: {0}")]
    Operation(String),
    /// The repository changed after the proposal was prepared, so it must be
    /// prepared again before it can be applied.
    #[error("proposal is stale: {0}")]
    Stale(String),
    /// A configured policy, such as a vulnerability severity gate, rejected
    /// the proposal.
    #[error("policy rejected proposal: {0}")]
    Policy(String),
    /// An I/O error not attributed to a more specific cause.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Result type used throughout depsmith.
pub type Result<T> = std::result::Result<T, Error>;

/// A manifest or workflow file that depsmith updates, as found by discovery.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Target {
    /// Stable identifier `manager:path`, such as `pixi:pixi.toml`.
    pub id: String,
    /// The package manager that owns the target, such as `pixi` or
    /// `github-actions`.
    pub manager: String,
    /// Path of the manifest or workflow file, relative to the repository root.
    pub manifest: PathBuf,
}

/// Options for preparing, scanning and applying updates. Explicit options
/// override `depsmith.toml`; unset fields keep the defaults shown by
/// [`UpdateOptions::default`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UpdateOptions {
    /// Direct dependencies to update; empty means the whole target.
    pub packages: Vec<String>,
    /// `NAME` or `NAME=REQUIREMENT`: accept a constraint suggestion.
    pub accept: Vec<String>,
    /// Allow the selected packages' declared constraints to change (an
    /// upgrade) instead of resolving within them (an update).
    pub upgrade: bool,
    /// Allow Git pins to move to newer commits.
    pub refresh_git: bool,
    /// Minimum release age in days; rejected where the adapter cannot enforce it.
    pub cooldown_days: Option<u32>,
    /// Also install the candidate's default environment for the current host.
    pub install: bool,
    /// Scan the baseline and candidate for known vulnerabilities.
    pub scan: bool,
    /// Lowest finding severity that rejects the proposal:
    /// `negligible`, `low`, `medium`, `high` or `critical`.
    pub fail_on: Option<String>,
    /// Apply `fail_on` only to findings the candidate introduces.
    pub only_new: bool,
    /// Executable paths by tool name (see each adapter's tool specs), such as
    /// `pixi` or `grype`; a tool without an entry runs its default.
    pub tools: BTreeMap<String, String>,
    /// Deprecated alias for `tools["pixi"]`.
    pub pixi: String,
    /// Deprecated alias for `tools["grype"]`.
    pub grype: String,
    /// Environment of tools `depsmith init` installed, by tool name, filled
    /// by the engine from the tool records; never configured.
    #[serde(skip)]
    pub tool_envs: BTreeMap<String, Vec<(String, String)>>,
    /// Time limit for each package-manager or scanner process, in seconds.
    pub timeout_seconds: u64,
    /// Reviewed identity mappings used when scanning.
    pub identity_mappings: Vec<crate::scan::IdentityMapping>,
    /// Scoped, documented suppressions applied to policy gates.
    pub suppressions: Vec<crate::scan::Suppression>,
}
impl Default for UpdateOptions {
    fn default() -> Self {
        Self {
            packages: vec![],
            accept: vec![],
            upgrade: false,
            refresh_git: false,
            cooldown_days: None,
            install: false,
            scan: false,
            fail_on: None,
            only_new: false,
            tools: BTreeMap::new(),
            pixi: "pixi".into(),
            grype: "grype".into(),
            tool_envs: BTreeMap::new(),
            timeout_seconds: 300,
            identity_mappings: vec![],
            suppressions: vec![],
        }
    }
}

/// An evidence-backed report that a declared constraint excludes a newer
/// release or is otherwise worth revisiting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Suggestion {
    /// Identifier of the target that declares the constraint.
    pub target: String,
    /// The constrained direct dependency.
    pub package: String,
    /// The declared requirement, as written in the manifest.
    pub requirement: String,
    /// Why the constraint is reported and how to act on it.
    pub reason: String,
    /// Where the suggestion's claim comes from (artifact records, release pages).
    #[serde(default)]
    pub evidence: Vec<String>,
}

/// One resolved package in an inventory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Package {
    /// Ecosystem of the package identity, such as `conda`, `pypi` or
    /// `github-actions`.
    pub ecosystem: String,
    /// Package name as the ecosystem spells it.
    pub name: String,
    /// Resolved version; empty when the lock does not record one.
    pub version: String,
    /// The exact artifact or reference resolved, such as a package URL or a
    /// workflow reference.
    pub artifact: String,
    /// Platform the package was resolved for, such as `linux-64`.
    pub platform: String,
}

/// A package that differs between the baseline and the candidate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyChange {
    /// The package before the update; `None` when it is newly added.
    pub before: Option<Package>,
    /// The package after the update; `None` when it is removed.
    pub after: Option<Package>,
    /// The declared dependencies whose dependency paths reach the package
    /// (in the candidate, or the baseline when removed); empty for a
    /// declared package or when the adapter reads no lock graph.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub introducers: Vec<String>,
    /// The shortest dependency path from each introducer to the package.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<Vec<String>>,
}

/// A target whose candidate could not be prepared.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Failure {
    /// Identifier of the failed target.
    pub target: String,
    /// What went wrong, including redacted backend output where available.
    pub message: String,
    /// Exit status category of the failure (see [`Error::exit_code`]).
    pub code: u8,
}

/// The exact new content of one file in a proposal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    /// Identifier of the target the file belongs to.
    pub target: String,
    /// Path relative to the repository root.
    pub path: PathBuf,
    /// Current content; `None` when the file does not exist yet.
    pub before: Option<String>,
    /// Content that applying the proposal writes.
    pub after: String,
    /// Unified diff from `before` to `after`, for review.
    pub diff: String,
}

/// A dependency reference left unchanged because it could not be resolved,
/// with the reason; distinct from references that are already current.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Unresolved {
    /// Identifier of the target containing the reference.
    pub target: String,
    /// The referenced package, such as an action repository.
    pub package: String,
    /// The reference as written, such as `org/repo@main`.
    pub reference: String,
    /// Why no release could be matched.
    pub reason: String,
}

/// The reviewable outcome of preparing one or more targets. A proposal is
/// applied exactly as prepared, and only while the repository still matches
/// the inputs it was prepared from.
#[derive(Debug, Serialize)]
pub struct Proposal {
    /// Version of the serialised report format.
    pub schema_version: u32,
    /// Canonical repository root the proposal was prepared for.
    pub root: PathBuf,
    /// Targets that were prepared.
    pub targets: Vec<Target>,
    /// Exact file changes that applying writes.
    pub changes: Vec<FileChange>,
    /// Packages that change between baseline and candidate, including
    /// transitive dependencies.
    pub dependencies: Vec<DependencyChange>,
    /// Suggestions about declared constraints.
    pub suggestions: Vec<Suggestion>,
    /// References left unchanged because they could not be resolved.
    pub unresolved: Vec<Unresolved>,
    /// Targets that could not be prepared.
    pub failures: Vec<Failure>,
    /// Validation levels completed and notes such as accepted suggestions.
    pub validation: Vec<String>,
    /// Vulnerability scan reports, when scanning was requested.
    pub scans: Vec<crate::scan::ScanReport>,
    #[serde(skip)]
    pub(crate) inputs: BTreeMap<PathBuf, String>,
    #[serde(skip)]
    pub(crate) layout: crate::working_tree::Layout,
}

/// What applying a proposal wrote.
#[derive(Debug, Serialize)]
pub struct ApplyResult {
    /// Version of the serialised report format.
    pub schema_version: u32,
    /// Files written, relative to the repository root.
    pub applied: Vec<PathBuf>,
    /// Whether only the successful targets of a partially failed proposal
    /// were applied.
    pub partial: bool,
}

impl Error {
    /// CLI exit status for this error: 2 for an invalid request, 4 for a
    /// policy rejection and 3 otherwise.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Invalid(_) => 2,
            Self::Policy(_) => 4,
            _ => 3,
        }
    }
}
impl Proposal {
    /// CLI exit status for this proposal. `check` selects check semantics,
    /// where pending changes give 1.
    ///
    /// With failures: 5 when only some targets failed (partial success),
    /// otherwise 3 if any failure is an operation failure, else 4 if any is a
    /// policy rejection, else 2. Without failures: 1 for a check with pending
    /// changes, otherwise 0.
    pub fn exit_code(&self, check: bool) -> u8 {
        if !self.failures.is_empty() {
            if self.failures.len() < self.targets.len() {
                return 5;
            }
            if self.failures.iter().any(|f| f.code == 3) {
                return 3;
            }
            if self.failures.iter().any(|f| f.code == 4) {
                return 4;
            }
            return 2;
        }
        if check && !self.changes.is_empty() {
            1
        } else {
            0
        }
    }
}

impl UpdateOptions {
    /// The environment the tool `name` runs with (empty unless installed by
    /// `depsmith init`); pass it to [`crate::process::run_env`].
    pub fn tool_env(&self, name: &str) -> &[(String, String)] {
        self.tool_envs.get(name).map_or(&[], Vec::as_slice)
    }

    /// The executable to run for the tool named `name`: its `tools` entry,
    /// else the deprecated `pixi`/`grype` field, else `name` itself.
    pub fn tool(&self, name: &str) -> String {
        if let Some(path) = self.tools.get(name) {
            return path.clone();
        }
        match name {
            "pixi" => self.pixi.clone(),
            "grype" => self.grype.clone(),
            _ => name.to_owned(),
        }
    }

    /// Check the options for consistency before any work starts: a positive
    /// timeout, well-formed acceptances and suppressions, a known `fail_on`
    /// severity used together with scanning, and `only_new` only with
    /// `fail_on`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] describing the first problem found.
    pub fn validate(&self) -> Result<()> {
        if self.timeout_seconds == 0 {
            return Err(Error::Invalid("timeout must be positive".into()));
        }
        if let Some((name, _)) = self
            .tools
            .iter()
            .find(|(name, path)| name.trim().is_empty() || path.trim().is_empty())
        {
            return Err(Error::Invalid(format!(
                "tool {name:?} needs a non-empty name and path"
            )));
        }
        for accept in &self.accept {
            crate::constraint::parse_accept(accept)?;
        }
        for suppression in &self.suppressions {
            suppression.validate()?;
        }
        if let Some(level) = &self.fail_on {
            if crate::scan::severity_rank(level).is_none() {
                return Err(Error::Invalid(
                    "severity must be negligible, low, medium, high or critical".into(),
                ));
            }
            if !self.scan {
                return Err(Error::Invalid(
                    "a vulnerability gate requires scanning".into(),
                ));
            }
        }
        if self.only_new && self.fail_on.is_none() {
            return Err(Error::Invalid("only_new requires fail_on".into()));
        }
        Ok(())
    }
}

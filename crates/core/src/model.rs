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
    /// Packages, direct or transitive, to move to an exact release without
    /// changing constraints. When any are given, only they (and any
    /// `packages`) move. Where the package manager has no precise update,
    /// each moves to the newest release its constraints allow instead, and
    /// the proposal says so.
    pub precise: Vec<PreciseUpdate>,
    /// `NAME` or `NAME=REQUIREMENT`: accept a constraint suggestion.
    pub accept: Vec<String>,
    /// Allow the selected packages' declared constraints to change (an
    /// upgrade) instead of resolving within them (an update).
    pub upgrade: bool,
    /// Allow Git pins to move to newer commits.
    pub refresh_git: bool,
    /// Propose a resolution that moves a package to an older version; by
    /// default such a target is held back (ADR 0008).
    pub allow_downgrades: bool,
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
    /// Lowest severity a security update acts on: `negligible`, `low`,
    /// `medium`, `high` or `critical`; every severity when unset.
    pub min_severity: Option<String>,
    /// Which fixed release a security update aims each vulnerable package at.
    pub security_target: SecurityTarget,
    /// Prepare a security update: move only packages with actionable
    /// findings, each toward its fix version. Implies `scan`; cannot be
    /// combined with `packages` or `upgrade`.
    pub security_only: bool,
}

/// Which release a security update aims a vulnerable package at, among the
/// releases its advisory names as fixed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SecurityTarget {
    /// The lowest fixed release above the locked version.
    #[default]
    Lowest,
    /// The newest release in the lowest fix's release line.
    Patch,
    /// The newest release the constraints allow.
    Newest,
}
impl Default for UpdateOptions {
    fn default() -> Self {
        Self {
            packages: vec![],
            precise: vec![],
            accept: vec![],
            upgrade: false,
            refresh_git: false,
            allow_downgrades: false,
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
            min_severity: None,
            security_target: SecurityTarget::Lowest,
            security_only: false,
        }
    }
}

/// A request to move one package to an exact release without changing any
/// constraint: a precise update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreciseUpdate {
    /// Package name as the lock spells it; it need not be declared.
    pub package: String,
    /// The exact release to move to.
    pub version: String,
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
    /// The environments that lock it, where the lock has several (Pixi);
    /// empty otherwise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub environments: Vec<String>,
}

impl Package {
    /// Whether `other` is the same resolution: everything but the
    /// environments that lock it, which change without the package moving.
    pub(crate) fn same_resolution(&self, other: &Package) -> bool {
        self.ecosystem == other.ecosystem
            && self.name == other.name
            && self.version == other.version
            && self.artifact == other.artifact
            && self.platform == other.platform
    }

    /// Whether `other` is a version of the same package: same ecosystem,
    /// name and platform.
    pub(crate) fn same_package(&self, other: &Package) -> bool {
        self.ecosystem == other.ecosystem
            && self.name == other.name
            && self.platform == other.platform
    }
}

/// A platform a lock names, where a lock lets platforms be named (Pixi
/// lock v7): what it is, by its conda subdir and virtual packages.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockPlatform {
    /// The name packages are locked under, such as `linux-cuda`.
    pub name: String,
    /// The conda subdir it resolves for, such as `linux-64`.
    pub subdir: String,
    /// The virtual packages it assumes, such as `__cuda=12`, sorted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub virtual_packages: Vec<String>,
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

impl DependencyChange {
    /// The packages that moved between two inventories. A lock can hold
    /// several versions of one package (Python forks, npm nesting, cargo
    /// majors, Pixi environments), so per package only versions missing from
    /// the other side are paired (see [`pair_versions`]); the rest are removed
    /// or added. Changes follow inventory order.
    pub(crate) fn between(before: &[Package], after: &[Package]) -> Vec<DependencyChange> {
        // npm lists each installed copy: copies of one resolution move as
        // one, locked in all their environments.
        let missing = |side: &[Package], other: &[Package]| -> Vec<Package> {
            let mut output: Vec<Package> = vec![];
            for p in side
                .iter()
                .filter(|p| !other.iter().any(|o| o.same_resolution(p)))
            {
                match output.iter_mut().find(|o| o.same_resolution(p)) {
                    Some(copy) => {
                        for e in &p.environments {
                            if !copy.environments.contains(e) {
                                copy.environments.push(e.clone());
                            }
                        }
                    }
                    None => output.push(p.clone()),
                }
            }
            output
        };
        let (mut gone, mut new) = (missing(before, after), missing(after, before));
        let mut changes = vec![];
        for package in before.iter().chain(after) {
            let olds = gone.extract_if(.., |p| p.same_package(package)).collect();
            let news = new.extract_if(.., |p| p.same_package(package)).collect();
            changes.extend(pair_versions(olds, news));
        }
        changes
    }
}

/// Pair the moved versions of one package: newest first, each old version
/// with the new one sharing most of its environments (on its own release
/// line when several share as many), then the rest within their release
/// line, then newest with newest. Nested npm copies carry no install path, so they pair by
/// version alone.
fn pair_versions(mut olds: Vec<Package>, mut news: Vec<Package>) -> Vec<DependencyChange> {
    let newest_first = |x: &Package, y: &Package| {
        crate::ecosystem::compare_versions(&x.ecosystem, &y.version, &x.version)
    };
    olds.sort_by(newest_first);
    news.sort_by(newest_first);
    let shared = |x: &Package, y: &Package| {
        x.environments
            .iter()
            .filter(|e| y.environments.contains(e))
            .count()
    };
    let change = |before, after| DependencyChange {
        before,
        after,
        introducers: vec![],
        paths: vec![],
    };
    // Copies on different release lines (npm nesting, cargo majors) move
    // independently, so a version prefers a successor on its own line.
    let same_line = |x: &Package, y: &Package| {
        crate::ecosystem::major_part(&x.version) == crate::ecosystem::major_part(&y.version)
    };
    let rank = |x: &Package, y: &Package| (shared(x, y), same_line(x, y));
    let mut changes = vec![];
    let mut unpaired = vec![];
    for old in olds {
        let best = (0..news.len())
            .filter(|&i| shared(&old, &news[i]) > 0)
            .fold(None, |best: Option<usize>, i| match best {
                Some(b) if rank(&old, &news[b]) >= rank(&old, &news[i]) => Some(b),
                _ => Some(i),
            });
        match best {
            Some(i) => changes.push(change(Some(old), Some(news.remove(i)))),
            None => unpaired.push(old),
        }
    }
    let mut leftover = vec![];
    for old in unpaired {
        match news.iter().position(|new| same_line(new, &old)) {
            Some(i) => changes.push(change(Some(old), Some(news.remove(i)))),
            None => leftover.push(old),
        }
    }
    let mut news = news.into_iter();
    for old in leftover {
        changes.push(change(Some(old), news.next()));
    }
    changes.extend(news.map(|new| change(None, Some(new))));
    changes
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
    /// Targets held back because their resolution would downgrade a package
    /// (ADR 0008); each downgrade is also an unresolved reference.
    pub held_back: Vec<String>,
    /// Targets that could not be prepared.
    pub failures: Vec<Failure>,
    /// Validation levels completed and notes such as accepted suggestions.
    pub validation: Vec<String>,
    /// Vulnerability scan reports, when scanning was requested.
    pub scans: Vec<crate::scan::ScanReport>,
    /// What a security update did about each finding it covers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub security: Vec<SecurityFix>,
    #[serde(skip)]
    pub(crate) inputs: BTreeMap<PathBuf, String>,
    #[serde(skip)]
    pub(crate) layout: crate::working_tree::Layout,
}

/// What a security update did about one finding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityFix {
    /// Identifier of the target the finding belongs to.
    pub target: String,
    /// Advisory identifier, such as `GHSA-…`.
    pub advisory: String,
    /// Severity as reported.
    pub severity: String,
    /// Ecosystem of the vulnerable package.
    pub ecosystem: String,
    /// Name of the vulnerable package.
    pub package: String,
    /// The locked version the finding matched.
    pub version: String,
    /// The release the update aimed at; `None` when the finding is unfixable.
    pub fix_version: Option<String>,
    /// Whether the finding was fixed, blocked or unfixable.
    pub outcome: FixOutcome,
    /// Why, for a blocked or unfixable finding.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    /// For a transitive package, the dependency paths from each declared
    /// dependency that reaches it, read from the lock graph; empty for a
    /// declared package or when no lock graph records them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<Vec<String>>,
}

/// The outcome of a security update for one finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FixOutcome {
    /// The candidate scan no longer reports it.
    Fixed,
    /// It has a fixed release the update could not reach; a suggestion
    /// names the declaration to change where there is one.
    Blocked,
    /// No release fixes it; nothing changed for it.
    Unfixable,
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
    /// policy rejection, else 2. Without failures: for a check, 6 when a
    /// target is held back (even if others have pending changes, so the hold
    /// is never hidden), else 1 with pending changes; otherwise 0.
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
        if check && !self.held_back.is_empty() {
            6
        } else if check && !self.changes.is_empty() {
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
    /// timeout, well-formed precise updates, acceptances and suppressions, a
    /// known `fail_on` severity used together with scanning, `only_new` only
    /// with `fail_on`, and a known `min_severity`.
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
        if let Some(request) = self
            .precise
            .iter()
            .find(|p| p.package.trim().is_empty() || p.version.trim().is_empty())
        {
            return Err(Error::Invalid(format!(
                "precise update {:?}={:?} needs a non-empty package and version",
                request.package, request.version
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
        if self
            .min_severity
            .as_deref()
            .is_some_and(|level| crate::scan::severity_rank(level).is_none())
        {
            return Err(Error::Invalid(
                "minimum severity must be negligible, low, medium, high or critical".into(),
            ));
        }
        if self.security_only && self.upgrade {
            return Err(Error::Invalid(
                "a security update never changes constraints; drop --upgrade".into(),
            ));
        }
        if self.security_only && !self.packages.is_empty() {
            return Err(Error::Invalid(
                "a security update chooses its packages from the findings; drop --package".into(),
            ));
        }
        if self.only_new && self.fail_on.is_none() {
            return Err(Error::Invalid("only_new requires fail_on".into()));
        }
        Ok(())
    }
}

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
    Supported,
    Unsupported(&'static str),
    NotApplicable,
}

/// The native executable an adapter drives. Only versions actually exercised
/// are listed; others are reported as untested, not assumed incompatible.
#[derive(Debug, Clone, Serialize)]
pub struct NativeTool {
    /// `UpdateOptions` field naming the executable.
    pub option: &'static str,
    pub tested_versions: &'static [&'static str],
}

#[derive(Debug, Clone, Serialize)]
pub struct Capabilities {
    pub manager: &'static str,
    pub package_selection: Support,
    pub constraint_changes: Support,
    /// Rewriting a declared requirement to accept a suggestion (`--accept`).
    pub suggestion_acceptance: Support,
    pub git_refresh: Support,
    pub cooldown: Support,
    pub install_validation: Support,
    pub lockfile: Support,
    pub platforms: Support,
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

#[derive(Default)]
pub struct Candidate {
    pub files: Vec<PathBuf>,
    pub suggestions: Vec<Suggestion>,
    pub unresolved: Vec<Unresolved>,
    pub before: Vec<Package>,
    pub baseline_available: bool,
    pub after: Vec<Package>,
    pub validation: Vec<String>,
}

/// Adapters only prepare inside a disposable stage. All source writes belong to the engine.
pub trait Adapter: Send + Sync {
    fn manager(&self) -> &'static str;
    fn detects(&self, relative: &Path, content: &str) -> bool;
    fn capabilities(&self) -> Capabilities {
        Capabilities::undeclared(self.manager())
    }
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
    fn prepare(&self, stage: &Path, target: &Target, options: &UpdateOptions) -> Result<Candidate>;
}

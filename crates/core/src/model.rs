use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid request: {0}")]
    Invalid(String),
    #[error("operation failed: {0}")]
    Operation(String),
    #[error("proposal is stale: {0}")]
    Stale(String),
    #[error("policy rejected proposal: {0}")]
    Policy(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Target {
    pub id: String,
    pub manager: String,
    pub manifest: PathBuf,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UpdateOptions {
    pub packages: Vec<String>,
    /// `NAME` or `NAME=REQUIREMENT`: accept a constraint suggestion.
    pub accept: Vec<String>,
    pub upgrade: bool,
    pub refresh_git: bool,
    pub cooldown_days: Option<u32>,
    pub install: bool,
    pub scan: bool,
    pub fail_on: Option<String>,
    pub only_new: bool,
    pub pixi: String,
    pub grype: String,
    pub timeout_seconds: u64,
    pub identity_mappings: Vec<crate::scan::IdentityMapping>,
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
            pixi: "pixi".into(),
            grype: "grype".into(),
            timeout_seconds: 300,
            identity_mappings: vec![],
            suppressions: vec![],
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Suggestion {
    pub target: String,
    pub package: String,
    pub requirement: String,
    pub reason: String,
    /// Where the suggestion's claim comes from (artifact records, release pages).
    #[serde(default)]
    pub evidence: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Package {
    pub ecosystem: String,
    pub name: String,
    pub version: String,
    pub artifact: String,
    pub platform: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyChange {
    pub before: Option<Package>,
    pub after: Option<Package>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Failure {
    pub target: String,
    pub message: String,
    pub code: u8,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub target: String,
    pub path: PathBuf,
    pub before: Option<String>,
    pub after: String,
    pub diff: String,
}
/// A dependency reference left unchanged because it could not be resolved,
/// with the reason; distinct from references that are already current.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Unresolved {
    pub target: String,
    pub package: String,
    pub reference: String,
    pub reason: String,
}
#[derive(Debug, Serialize)]
pub struct Proposal {
    pub schema_version: u32,
    pub root: PathBuf,
    pub targets: Vec<Target>,
    pub changes: Vec<FileChange>,
    pub dependencies: Vec<DependencyChange>,
    pub suggestions: Vec<Suggestion>,
    pub unresolved: Vec<Unresolved>,
    pub failures: Vec<Failure>,
    pub validation: Vec<String>,
    pub scans: Vec<crate::scan::ScanReport>,
    #[serde(skip)]
    pub(crate) inputs: BTreeMap<PathBuf, String>,
}
#[derive(Debug, Serialize)]
pub struct ApplyResult {
    pub schema_version: u32,
    pub applied: Vec<PathBuf>,
    pub partial: bool,
}

impl Error {
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Invalid(_) => 2,
            Self::Policy(_) => 4,
            _ => 3,
        }
    }
}
impl Proposal {
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
    pub fn validate(&self) -> Result<()> {
        if self.timeout_seconds == 0 {
            return Err(Error::Invalid("timeout must be positive".into()));
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

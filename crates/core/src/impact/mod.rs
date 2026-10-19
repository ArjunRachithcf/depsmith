//! Impact analysis of a lock change (ADR 0006): what moved between two
//! resolutions of a target, the packages it affects, and the two sides of a
//! comparison read from git or a file.
//!
//! ```no_run
//! use depsmith_core::{adapter::Adapter, impact, uv::Uv, Target};
//! use std::path::Path;
//!
//! let root = Path::new(".");
//! let target = Target {
//!     id: "uv:pyproject.toml".into(),
//!     manager: "uv".into(),
//!     manifest: "pyproject.toml".into(),
//! };
//! let sides = impact::sides(root, &Uv::default(), &target, &impact::Comparison::default_for_env())?;
//! let lock = |side: &Path| impact::Lock::read(&Uv::default(), side, &target);
//! let declarations = Uv::default().declarations(sides.after(), &target)?;
//! let result = impact::affected(
//!     &lock(sides.before())?,
//!     &lock(sides.after())?,
//!     &declarations,
//!     &[],
//! );
//! for package in result.packages.iter().filter(|p| p.declared) {
//!     println!("{} is affected", package.name);
//! }
//! # Ok::<(), depsmith_core::Error>(())
//! ```
mod change;
mod compare;
mod tables;

pub use change::{
    affected, lock_changes, Affected, AffectedPackage, ChangeKind, FullRun, Lock, LockChange,
    Reached,
};
pub use compare::{sides, Comparison, Sides};
pub use tables::{
    BUILD_TOOLS, COMPILER_PREFIXES, INTERPRETERS, PYTHON_STDLIB_MAP, RUNTIME_BARRIERS,
};

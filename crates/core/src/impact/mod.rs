//! Impact analysis of a lock change (ADR 0006): what moved between two
//! resolutions of a target, the packages it affects, the two sides of a
//! comparison read from git or a file, and the usage sites and affected
//! files in the target's own code, found by the import scanners.
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
//! let mut sides = impact::sides(root, &Uv::default(), &target, &impact::Comparison::default_for_env())?;
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
//! let names = impact::ImportNames::read(root, &target);
//! for site in impact::usages(sides.sources()?, &target, &result, &names)?.sites {
//!     println!("{}:{} imports {}", site.file.display(), site.line, site.module);
//! }
//! # Ok::<(), depsmith_core::Error>(())
//! ```
mod change;
mod compare;
mod imports;
mod js;
mod names;
mod python;
mod specifiers;
mod tables;
mod usage;

pub use change::{
    affected, lock_changes, Affected, AffectedPackage, ChangeKind, FullRun, Lock, LockChange,
    Reached,
};
pub use compare::{sides, Comparison, Sides};
pub use imports::{Import, ImportKind, Scan};
pub use js::{js_imports, Dialect};
pub use names::ImportNames;
pub use python::python_imports;
pub use tables::{
    BUILD_TOOLS, COMPILER_PREFIXES, INTERPRETERS, PYTHON_IMPORT_NAMES, PYTHON_NAMESPACES,
    PYTHON_STDLIB_MAP, RUNTIME_BARRIERS,
};
pub use usage::{usages, AffectedFile, Hop, UsageSite, Usages};

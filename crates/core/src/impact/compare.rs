//! The two sides of a comparison: the target's lock and manifest files before
//! and after a change, read from git, a file or the worktree.
use crate::{adapter::Adapter, scm, Error, Result, Target};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Which two states of a target a lock change is between.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Comparison {
    /// The index against `HEAD`: what the next commit changes.
    Staged,
    /// The worktree against `base`, by default the merge-base of `HEAD` with
    /// the default branch.
    Worktree {
        /// The revision to compare with; `None` for the merge-base.
        base: Option<String>,
    },
    /// The worktree against the index: what is not staged yet.
    Unstaged,
    /// Revision `head` against revision `base`.
    Revisions {
        /// The "before" revision.
        base: String,
        /// The "after" revision.
        head: String,
    },
    /// The worktree against an old copy of the target's lock.
    OldLockfile(PathBuf),
}

impl Comparison {
    /// The default comparison: the worktree against the merge-base in CI
    /// (`ci`), the staged change otherwise.
    pub fn default_for(ci: bool) -> Self {
        if ci {
            Self::Worktree { base: None }
        } else {
            Self::Staged
        }
    }

    /// The default comparison here: [`Comparison::default_for`], in CI when
    /// the `CI` environment variable is set.
    pub fn default_for_env() -> Self {
        Self::default_for(std::env::var_os("CI").is_some())
    }
}

/// A directory holding one side of a comparison.
#[derive(Debug)]
enum Side {
    /// The worktree itself.
    Worktree(PathBuf),
    /// The target's files, written to a temporary directory.
    Copy(TempDir),
}

impl Side {
    fn path(&self) -> &Path {
        match self {
            Self::Worktree(path) => path,
            Self::Copy(dir) => dir.path(),
        }
    }
}

/// The two sides of a comparison, each a root that an adapter's
/// `lock_graph`, `inventory` and `declarations` read the target from.
/// Temporary sides are removed on drop.
#[derive(Debug)]
pub struct Sides {
    before: Side,
    after: Side,
    /// Where the "after" side's source code is read from, until
    /// [`Sides::sources`] has read it.
    sources: Option<Sources>,
}

/// The target's directory in git: at `revision`, or in the index when `None`.
#[derive(Debug)]
struct Sources {
    root: PathBuf,
    revision: Option<String>,
    dir: PathBuf,
}

impl Sides {
    /// The root of the "before" side.
    pub fn before(&self) -> &Path {
        self.before.path()
    }

    /// The root of the "after" side, where manifests are read from.
    pub fn after(&self) -> &Path {
        self.after.path()
    }

    /// The root of the "after" side with the target's whole directory in it,
    /// source code included. A side read from git (the index, or `head`)
    /// holds only the target's manifest and its inputs until this is called;
    /// then every regular file of the directory is written as git stores it,
    /// with the ignore files above it, and without attributes, filters,
    /// symlinks or submodules.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Invalid`] when git fails.
    pub fn sources(&mut self) -> Result<&Path> {
        if let Some(sources) = &self.sources {
            scm::materialize_tree(
                &sources.root,
                sources.revision.as_deref(),
                &sources.dir,
                self.after.path(),
            )?;
            self.sources = None;
        }
        Ok(self.after.path())
    }
}

/// The two sides of `comparison` for `target` under `root`, the repository
/// or a directory in it.
///
/// A side that is not the worktree holds only the target's files: its
/// manifest and the inputs `adapter` manages beside it, such as its lock, as
/// git has them. Files of other manifests, such as uv workspace members, are
/// not included, so declarations are read from the "after" side.
/// [`Sides::sources`] adds the rest of the target's directory to the "after"
/// side.
/// [`Comparison::OldLockfile`] writes the old lock over a copy of the
/// worktree's files, at the first managed input named like a lock that
/// exists, else the first one.
///
/// # Errors
///
/// Returns [`crate::Error::Invalid`] when git fails, such as for an unknown
/// revision or outside a repository, when there is no default branch for the
/// merge-base, or when the old lock cannot be read.
pub fn sides(
    root: &Path,
    adapter: &dyn Adapter,
    target: &Target,
    comparison: &Comparison,
) -> Result<Sides> {
    let files = files(adapter, target);
    let revision = |revision: Option<&str>| -> Result<Side> {
        let dir = tempfile::tempdir()?;
        scm::materialize(root, revision, &files, dir.path())?;
        Ok(Side::Copy(dir))
    };
    let worktree = || Side::Worktree(root.to_owned());
    let (before, after) = match comparison {
        Comparison::Staged => (revision(Some("HEAD"))?, revision(None)?),
        Comparison::Unstaged => (revision(None)?, worktree()),
        Comparison::Worktree { base } => {
            let base = match base {
                Some(base) => base.clone(),
                None => scm::merge_base(root)?,
            };
            (revision(Some(&base))?, worktree())
        }
        Comparison::Revisions { base, head } => (revision(Some(base))?, revision(Some(head))?),
        Comparison::OldLockfile(old) => {
            let text = std::fs::read(old).map_err(|e| {
                Error::Invalid(format!("cannot read old lockfile {}: {e}", old.display()))
            })?;
            let dir = tempfile::tempdir()?;
            for file in files.iter().filter(|f| root.join(f).is_file()) {
                scm::write_under(dir.path(), file, std::fs::read(root.join(file))?)?;
            }
            let lock = lock_files(adapter, target)
                .into_iter()
                .min_by_key(|f| !root.join(f).is_file())
                .ok_or_else(|| {
                    Error::Invalid(format!("{} has no lockfile to replace", target.id))
                })?;
            scm::write_under(dir.path(), &lock, text)?;
            (Side::Copy(dir), worktree())
        }
    };
    let sources = match comparison {
        Comparison::Staged => Some(None),
        Comparison::Revisions { head, .. } => Some(Some(head.clone())),
        _ => None,
    }
    .map(|revision| Sources {
        root: root.to_owned(),
        revision,
        dir: target.manifest.parent().unwrap_or(Path::new("")).to_owned(),
    });
    Ok(Sides {
        before,
        after,
        sources,
    })
}

/// The inputs `adapter` manages beside `target`'s manifest that are named
/// like a lock.
pub(crate) fn lock_files(adapter: &dyn Adapter, target: &Target) -> Vec<PathBuf> {
    files(adapter, target)
        .into_iter()
        .skip(1)
        .filter(|f| {
            let name = f.file_name().unwrap_or_default().to_string_lossy();
            name.contains("lock") || name.contains("shrinkwrap")
        })
        .collect()
}

/// The target's manifest, then the inputs its adapter manages beside it.
fn files(adapter: &dyn Adapter, target: &Target) -> Vec<PathBuf> {
    let name = target
        .manifest
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let dir = target.manifest.parent().unwrap_or(Path::new(""));
    let mut files = vec![target.manifest.clone()];
    for rule in adapter.spec().managed {
        if rule
            .manifests
            .iter()
            .any(|pattern| crate::working_tree::matches_pattern(pattern, &name))
        {
            for input in &rule.inputs {
                let file = dir.join(input);
                if !files.contains(&file) {
                    files.push(file);
                }
            }
        }
    }
    files
}

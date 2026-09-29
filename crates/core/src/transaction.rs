//! Applying proposals: stale-input checks, an operation lock, and a journal
//! that makes multi-file writes recoverable.
use crate::{working_tree, ApplyResult, Error, FileChange, Proposal, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema_version: u32,
    changes: Vec<FileChange>,
}

fn safe_path(root: &Path, path: &Path) -> Result<()> {
    working_tree::relative(path)?;
    let mut current = root.to_path_buf();
    for part in path.components() {
        current.push(part);
        if let Ok(meta) = fs::symlink_metadata(&current) {
            if meta.file_type().is_symlink() {
                return Err(Error::Invalid("symlink in application path".into()));
            }
        }
    }
    Ok(())
}
fn atomic_write(path: &Path, content: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Invalid("missing parent directory".into()))?;
    fs::create_dir_all(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    if let Ok(meta) = fs::metadata(path) {
        tmp.as_file().set_permissions(meta.permissions())?;
    }
    tmp.write_all(content.as_bytes())?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| Error::Io(e.error))?;
    Ok(())
}
fn content(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Exclusive for the life of the returned handle, across processes. The OS
/// releases it if the holder crashes, so a stale lock cannot wedge recovery.
fn operation_lock(root: &Path) -> Result<fs::File> {
    safe_path(root, Path::new(".depsmith/lock"))?;
    fs::create_dir_all(root.join(".depsmith"))?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join(".depsmith/lock"))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(fs::TryLockError::WouldBlock) => Err(Error::Operation(
            "another depsmith operation is in progress in this repository".into(),
        )),
        Err(fs::TryLockError::Error(error)) => Err(error.into()),
    }
}

/// Publish the journal only once it is complete and durable: write a temporary
/// file, sync it, then hard-link it into place (atomic, and failing if an
/// interrupted operation's journal already exists).
fn write_journal(root: &Path, journal: &Journal) -> Result<()> {
    let directory = root.join(".depsmith");
    let bytes = serde_json::to_vec(journal).map_err(|e| Error::Operation(e.to_string()))?;
    let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    fs::hard_link(temporary.path(), directory.join("journal.json")).map_err(|e| {
        Error::Operation(format!(
            "cannot acquire application journal (run recover for an interrupted operation): {e}"
        ))
    })?;
    #[cfg(unix)]
    fs::File::open(&directory)?.sync_all()?;
    Ok(())
}

/// Write exactly the files of a reviewed proposal, without resolving again.
///
/// Every file must still hold the content the proposal was prepared from, and
/// the repository's recorded inputs must be unchanged; otherwise nothing is
/// written and the proposal is stale. Writes are journaled so that an
/// interrupted apply can be completed or undone with [`recover`]. A proposal
/// with failures is applied only when `allow_partial` is set, and then only
/// for the targets that succeeded.
///
/// ```
/// use depsmith_core::{apply, Engine, Error, UpdateOptions};
///
/// let repo = tempfile::tempdir()?;
/// std::fs::write(repo.path().join("pixi.toml"), "[workspace]\nname = 'demo'\n")?;
/// let options = UpdateOptions { pixi: "no-such-pixi".into(), ..Default::default() };
/// let proposal = Engine::default().prepare(repo.path(), &["pixi:pixi.toml".into()], options)?;
/// // A proposal with failed targets needs explicit partial application.
/// assert!(matches!(apply(&proposal, false), Err(Error::Operation(_))));
/// # Ok::<(), Error>(())
/// ```
///
/// # Errors
///
/// Returns [`Error::Stale`] when the repository changed since preparation,
/// [`Error::Operation`] for failed targets without `allow_partial` or when
/// another operation holds the repository lock, and I/O errors from writing.
pub fn apply(proposal: &Proposal, allow_partial: bool) -> Result<ApplyResult> {
    if !proposal.failures.is_empty() && !allow_partial {
        return Err(Error::Operation(
            "some targets failed; explicit partial application required".into(),
        ));
    }
    let root = &proposal.root;
    let _lock = operation_lock(root)?;
    for change in &proposal.changes {
        working_tree::output_path(root, &change.path)?;
        if content(&root.join(&change.path))? != change.before {
            return Err(Error::Stale(change.path.display().to_string()));
        }
    }
    if working_tree::fingerprint(root)? != proposal.inputs {
        return Err(Error::Stale(
            "repository inputs changed since preparation".into(),
        ));
    }
    if proposal.changes.is_empty() {
        return Ok(ApplyResult {
            schema_version: 1,
            applied: vec![],
            partial: !proposal.failures.is_empty(),
        });
    }
    safe_path(root, Path::new(".depsmith/journal.json"))?;
    let journal_path = root.join(".depsmith/journal.json");
    write_journal(
        root,
        &Journal {
            schema_version: 1,
            changes: proposal.changes.clone(),
        },
    )?;
    for change in &proposal.changes {
        working_tree::output_path(root, &change.path)?;
        if content(&root.join(&change.path))? != change.before {
            return Err(Error::Stale(
                "file changed during apply; recover before retrying".into(),
            ));
        }
        if let Err(error) = atomic_write(&root.join(&change.path), &change.after) {
            return Err(Error::Operation(format!(
                "apply interrupted; run recover: {error}"
            )));
        }
    }
    fs::remove_file(journal_path)?;
    Ok(ApplyResult {
        schema_version: 1,
        applied: proposal.changes.iter().map(|c| c.path.clone()).collect(),
        partial: !proposal.failures.is_empty(),
    })
}

/// Restore an interrupted operation only if all files still contain old or proposed bytes.
pub fn recover(root: &Path) -> Result<Vec<std::path::PathBuf>> {
    let root = working_tree::canonical(root)?;
    let _lock = operation_lock(&root)?;
    safe_path(&root, Path::new(".depsmith/journal.json"))?;
    let journal_path = root.join(".depsmith/journal.json");
    let bytes = match fs::read(&journal_path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::Invalid("no interrupted operation to recover".into()))
        }
        Err(e) => return Err(e.into()),
    };
    let journal: Journal = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Invalid(format!("invalid recovery journal: {e}")))?;
    if journal.schema_version != 1 {
        return Err(Error::Invalid("unknown journal version".into()));
    }
    for change in &journal.changes {
        working_tree::output_path(&root, &change.path)?;
        if change.path.starts_with(".depsmith") || change.path.starts_with(".git") {
            return Err(Error::Invalid("journal targets internal metadata".into()));
        }
        let current = content(&root.join(&change.path))?;
        if current != change.before && current.as_deref() != Some(&change.after) {
            return Err(Error::Stale(format!(
                "recovery would overwrite external edit: {}",
                change.path.display()
            )));
        }
    }
    for change in &journal.changes {
        let path = root.join(&change.path);
        if let Some(before) = &change.before {
            atomic_write(&path, before)?;
        } else if path.exists() {
            fs::remove_file(path)?;
        }
    }
    fs::remove_file(journal_path)?;
    Ok(journal.changes.into_iter().map(|c| c.path).collect())
}

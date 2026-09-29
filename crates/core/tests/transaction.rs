//! Applying proposals: stale inputs, shared files, partial application,
//! locking and recovery of interrupted writes.
use depsmith_core::{
    adapter::{Adapter, Candidate},
    apply, recover, Engine, Error, Proposal, Result, Target, UpdateOptions,
};
use std::{fs, path::Path};
use tempfile::{tempdir, TempDir};

/// Writes two candidate files, one of them in a subdirectory.
struct TwoFiles;
impl Adapter for TwoFiles {
    fn spec(&self) -> depsmith_core::adapter::AdapterSpec {
        depsmith_core::adapter::AdapterSpec::new("fixture", &["project.toml"])
    }
    fn detects(&self, path: &Path, _: &str) -> bool {
        path == Path::new("project.toml")
    }
    fn prepare(&self, root: &Path, _: &Target, _: &UpdateOptions) -> Result<Candidate> {
        fs::write(root.join("a.lock"), "new a\n")?;
        fs::write(root.join("sub/b.lock"), "new b\n")?;
        Ok(Candidate {
            files: vec!["a.lock".into(), "sub/b.lock".into()],
            ..Default::default()
        })
    }
}

fn proposal() -> (TempDir, Proposal) {
    let root = tempdir().unwrap();
    fs::create_dir(root.path().join("sub")).unwrap();
    fs::write(root.path().join("project.toml"), "").unwrap();
    fs::write(root.path().join("a.lock"), "old a\n").unwrap();
    fs::write(root.path().join("sub/b.lock"), "old b\n").unwrap();
    let proposal = Engine::new(vec![Box::new(TwoFiles)])
        .prepare(
            root.path(),
            &["fixture:project.toml".into()],
            UpdateOptions::default(),
        )
        .unwrap();
    (root, proposal)
}

fn read(root: &Path, path: &str) -> String {
    fs::read_to_string(root.join(path)).unwrap()
}

#[test]
fn apply_and_recover_exclude_each_other() {
    let (root, proposal) = proposal();
    fs::create_dir_all(root.path().join(".depsmith")).unwrap();
    // Another process holding the operation lock.
    let holder = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.path().join(".depsmith/lock"))
        .unwrap();
    holder.try_lock().unwrap();
    for error in [
        apply(&proposal, false).unwrap_err(),
        recover(root.path()).unwrap_err(),
    ] {
        assert!(
            error.to_string().contains("another depsmith operation"),
            "{error}"
        );
    }
    assert_eq!(read(root.path(), "a.lock"), "old a\n");
    drop(holder);
    apply(&proposal, false).unwrap();
    assert_eq!(read(root.path(), "a.lock"), "new a\n");
}

#[cfg(unix)]
#[test]
fn interrupted_apply_leaves_a_complete_journal_that_recovers() {
    use std::os::unix::fs::PermissionsExt;
    let (root, proposal) = proposal();
    let sub = root.path().join("sub");
    // Writing the second file fails after the first was replaced.
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o555)).unwrap();
    let error = apply(&proposal, false).unwrap_err();
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(error.to_string().contains("run recover"), "{error}");
    assert_eq!(read(root.path(), "a.lock"), "new a\n");
    let journal = read(root.path(), ".depsmith/journal.json");
    serde_json::from_str::<serde_json::Value>(&journal).expect("journal is complete JSON");
    // A new apply must not proceed over the interrupted one.
    assert!(apply(&proposal, false).is_err());
    let restored = recover(root.path()).unwrap();
    assert_eq!(restored.len(), 2);
    assert_eq!(read(root.path(), "a.lock"), "old a\n");
    assert_eq!(read(root.path(), "sub/b.lock"), "old b\n");
    assert!(!root.path().join(".depsmith/journal.json").exists());
}

#[test]
fn recover_without_an_interrupted_operation_is_a_clear_no_op_error() {
    let (root, _) = proposal();
    let error = recover(root.path()).unwrap_err();
    assert!(
        matches!(&error, Error::Invalid(m) if m.contains("no interrupted operation")),
        "{error}"
    );
}

#[test]
fn leftover_temporary_journal_does_not_block_application() {
    let (root, proposal) = proposal();
    // A crash while writing a journal can only leave an unnamed temporary file.
    fs::create_dir_all(root.path().join(".depsmith")).unwrap();
    fs::write(root.path().join(".depsmith/.tmpJ0urnal"), "{\"schema_ver").unwrap();
    apply(&proposal, false).unwrap();
    assert_eq!(read(root.path(), "sub/b.lock"), "new b\n");
}

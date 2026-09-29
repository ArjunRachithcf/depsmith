//! Safety of preparing: discovery boundaries and never modifying the
//! original repository.
use depsmith_core::{discover, Engine, UpdateOptions};
use std::fs;
use tempfile::tempdir;

#[test]
fn discovery_finds_nested_pixi_but_skips_environments() {
    let root = tempdir().unwrap();
    fs::create_dir_all(root.path().join("nested")).unwrap();
    fs::create_dir_all(root.path().join(".venv")).unwrap();
    fs::write(
        root.path().join("nested/pixi.toml"),
        "[workspace]\nname='demo'\n",
    )
    .unwrap();
    fs::write(
        root.path().join(".venv/pixi.toml"),
        "[workspace]\nname='ignore'\n",
    )
    .unwrap();
    let targets = discover(root.path()).unwrap();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].id, "pixi:nested/pixi.toml");
}

#[test]
fn failed_backend_never_modifies_original_manifest() {
    let root = tempdir().unwrap();
    let manifest = "[workspace]\nname='demo'\nplatforms=['linux-64']\n";
    fs::write(root.path().join("pixi.toml"), manifest).unwrap();
    let options = UpdateOptions {
        pixi: "depsmith-nonexistent-executable".into(),
        ..Default::default()
    };
    let proposal = Engine::default()
        .prepare(root.path(), &["pixi:pixi.toml".into()], options)
        .unwrap();
    assert_eq!(proposal.failures.len(), 1);
    assert!(proposal.changes.is_empty());
    assert_eq!(
        fs::read_to_string(root.path().join("pixi.toml")).unwrap(),
        manifest
    );
}

struct FixtureAdapter;
impl depsmith_core::adapter::Adapter for FixtureAdapter {
    fn manager(&self) -> &'static str {
        "fixture"
    }
    fn detects(&self, path: &std::path::Path, _: &str) -> bool {
        path == std::path::Path::new("project.toml")
    }
    fn prepare(
        &self,
        root: &std::path::Path,
        _: &depsmith_core::Target,
        _: &UpdateOptions,
    ) -> depsmith_core::Result<depsmith_core::adapter::Candidate> {
        fs::write(root.join("project.lock"), "reviewed candidate\n")?;
        Ok(depsmith_core::adapter::Candidate {
            files: vec!["project.lock".into()],
            ..Default::default()
        })
    }
}
fn proposal() -> (tempfile::TempDir, depsmith_core::Proposal) {
    let root = tempdir().unwrap();
    fs::write(root.path().join("project.toml"), "user's dirty content\n").unwrap();
    fs::write(root.path().join("project.lock"), "old\n").unwrap();
    let engine = Engine::new(vec![Box::new(FixtureAdapter)]);
    let result = engine
        .prepare(
            root.path(),
            &["fixture:project.toml".into()],
            UpdateOptions::default(),
        )
        .unwrap();
    (root, result)
}
#[test]
fn apply_uses_exact_candidate_and_preserves_user_changes() {
    let (root, proposal) = proposal();
    assert_eq!(
        fs::read_to_string(root.path().join("project.lock")).unwrap(),
        "old\n"
    );
    depsmith_core::apply(&proposal, false).unwrap();
    assert_eq!(
        fs::read_to_string(root.path().join("project.lock")).unwrap(),
        "reviewed candidate\n"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("project.toml")).unwrap(),
        "user's dirty content\n"
    );
}
#[test]
fn changed_source_invalidates_proposal_without_writes() {
    let (root, proposal) = proposal();
    fs::write(root.path().join("project.toml"), "concurrent edit\n").unwrap();
    assert!(depsmith_core::apply(&proposal, false).is_err());
    assert_eq!(
        fs::read_to_string(root.path().join("project.lock")).unwrap(),
        "old\n"
    );
}
#[test]
fn partial_application_requires_explicit_opt_in() {
    let (root, mut proposal) = proposal();
    proposal.failures.push(depsmith_core::Failure {
        target: "other".into(),
        message: "failed".into(),
        code: 3,
    });
    assert!(depsmith_core::apply(&proposal, false).is_err());
    assert_eq!(
        fs::read_to_string(root.path().join("project.lock")).unwrap(),
        "old\n"
    );
    assert!(depsmith_core::apply(&proposal, true).unwrap().partial);
}
#[test]
fn malicious_output_path_is_never_written() {
    let (_root, mut proposal) = proposal();
    proposal.changes[0].path = "../escape.lock".into();
    assert!(depsmith_core::apply(&proposal, false).is_err());
}
#[cfg(unix)]
#[test]
fn symlink_swap_is_rejected() {
    let (root, proposal) = proposal();
    let outside = tempdir().unwrap();
    fs::write(outside.path().join("secret"), "outside").unwrap();
    fs::remove_file(root.path().join("project.lock")).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("secret"),
        root.path().join("project.lock"),
    )
    .unwrap();
    assert!(depsmith_core::apply(&proposal, false).is_err());
    assert_eq!(
        fs::read_to_string(outside.path().join("secret")).unwrap(),
        "outside"
    );
}

struct SharedOutput;
impl depsmith_core::adapter::Adapter for SharedOutput {
    fn manager(&self) -> &'static str {
        "shared"
    }
    fn detects(&self, path: &std::path::Path, _: &str) -> bool {
        path.extension().is_some_and(|ext| ext == "toml")
    }
    fn prepare(
        &self,
        root: &std::path::Path,
        target: &depsmith_core::Target,
        _: &UpdateOptions,
    ) -> depsmith_core::Result<depsmith_core::adapter::Candidate> {
        fs::write(root.join("shared.lock"), &target.id)?;
        Ok(depsmith_core::adapter::Candidate {
            files: vec!["shared.lock".into()],
            ..Default::default()
        })
    }
}
#[test]
fn conflicting_targets_cannot_produce_an_applicable_partial_proposal() {
    let root = tempdir().unwrap();
    fs::write(root.path().join("a.toml"), "").unwrap();
    fs::write(root.path().join("b.toml"), "").unwrap();
    let result = Engine::new(vec![Box::new(SharedOutput)]).prepare(
        root.path(),
        &["shared:a.toml".into(), "shared:b.toml".into()],
        UpdateOptions::default(),
    );
    assert!(
        result.is_err(),
        "conflicting ownership must invalidate the proposal"
    );
    assert!(!root.path().join("shared.lock").exists());
}
#[test]
fn direct_engine_call_cannot_silently_skip_a_requested_vulnerability_gate() {
    let root = tempdir().unwrap();
    fs::write(root.path().join("project.toml"), "").unwrap();
    let options = UpdateOptions {
        fail_on: Some("high".into()),
        scan: false,
        ..Default::default()
    };
    let result = Engine::new(vec![Box::new(FixtureAdapter)]).prepare(
        root.path(),
        &["fixture:project.toml".into()],
        options,
    );
    assert!(matches!(result, Err(depsmith_core::Error::Invalid(_))));
}

#[test]
fn application_cannot_target_internal_metadata() {
    let (root, mut proposal) = proposal();
    proposal.changes[0].path = ".git/config".into();
    proposal.changes[0].before = None;
    assert!(depsmith_core::apply(&proposal, false).is_err());
    assert!(!root.path().join(".git/config").exists());
}

#[cfg(unix)]
#[test]
fn adapter_output_symlinks_are_rejected_before_reading() {
    struct LinkedOutput;
    impl depsmith_core::adapter::Adapter for LinkedOutput {
        fn manager(&self) -> &'static str {
            "linked"
        }
        fn detects(&self, _: &std::path::Path, _: &str) -> bool {
            true
        }
        fn prepare(
            &self,
            root: &std::path::Path,
            _: &depsmith_core::Target,
            _: &UpdateOptions,
        ) -> depsmith_core::Result<depsmith_core::adapter::Candidate> {
            std::os::unix::fs::symlink(root.join("project.toml"), root.join("output.lock"))?;
            Ok(depsmith_core::adapter::Candidate {
                files: vec!["output.lock".into()],
                ..Default::default()
            })
        }
    }
    let root = tempdir().unwrap();
    fs::write(root.path().join("project.toml"), "source").unwrap();
    let proposal = Engine::new(vec![Box::new(LinkedOutput)])
        .prepare(
            root.path(),
            &["linked:project.toml".into()],
            UpdateOptions::default(),
        )
        .unwrap();
    assert!(proposal.changes.is_empty());
    assert_eq!(proposal.failures.len(), 1);
}

#[test]
fn recovery_restores_old_bytes_and_removes_new_files() {
    let (root, proposal) = proposal();
    let mut changes = proposal.changes.clone();
    changes.push(depsmith_core::FileChange {
        target: "fixture:project.toml".into(),
        path: "new.lock".into(),
        before: None,
        after: "new".into(),
        diff: String::new(),
    });
    for change in &changes {
        fs::write(root.path().join(&change.path), &change.after).unwrap();
    }
    fs::create_dir(root.path().join(".depsmith")).unwrap();
    fs::write(
        root.path().join(".depsmith/journal.json"),
        serde_json::to_vec(&serde_json::json!({"schema_version":1,"changes":changes})).unwrap(),
    )
    .unwrap();
    assert_eq!(depsmith_core::recover(root.path()).unwrap().len(), 2);
    assert_eq!(
        fs::read_to_string(root.path().join("project.lock")).unwrap(),
        "old\n"
    );
    assert!(!root.path().join("new.lock").exists());
    assert!(!root.path().join(".depsmith/journal.json").exists());
}

#[test]
fn recovery_refuses_to_overwrite_concurrent_edits() {
    let (root, proposal) = proposal();
    fs::write(root.path().join("project.lock"), "external edit").unwrap();
    fs::create_dir(root.path().join(".depsmith")).unwrap();
    fs::write(
        root.path().join(".depsmith/journal.json"),
        serde_json::to_vec(&serde_json::json!({"schema_version":1,"changes":proposal.changes}))
            .unwrap(),
    )
    .unwrap();
    assert!(depsmith_core::recover(root.path()).is_err());
    assert_eq!(
        fs::read_to_string(root.path().join("project.lock")).unwrap(),
        "external edit"
    );
    assert!(root.path().join(".depsmith/journal.json").exists());
}

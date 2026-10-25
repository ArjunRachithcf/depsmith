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
    fn spec(&self) -> depsmith_core::adapter::AdapterSpec {
        depsmith_core::adapter::AdapterSpec::new("fixture", &["project.toml"])
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
    fn spec(&self) -> depsmith_core::adapter::AdapterSpec {
        depsmith_core::adapter::AdapterSpec::new("shared", &["*.toml"])
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
        fn spec(&self) -> depsmith_core::adapter::AdapterSpec {
            depsmith_core::adapter::AdapterSpec::new("linked", &["*"])
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

/// Records what the stage holds at `LICENSE` and `pkg/target/mod.py`, and
/// writes `project.lock`.
#[cfg(unix)]
struct StageProbe;
#[cfg(unix)]
impl depsmith_core::adapter::Adapter for StageProbe {
    fn spec(&self) -> depsmith_core::adapter::AdapterSpec {
        depsmith_core::adapter::AdapterSpec::new("probe", &["project.toml"])
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
        let link = root.join("crate/LICENSE");
        let seen = match fs::symlink_metadata(&link) {
            Ok(meta) if meta.file_type().is_symlink() => format!(
                "link {} {}",
                fs::read_link(&link)?.display(),
                fs::read_to_string(&link)?
            ),
            Ok(_) => "file".into(),
            Err(_) => "missing".into(),
        };
        let tracked = fs::read_to_string(root.join("pkg/target/mod.py")).unwrap_or_default();
        fs::write(root.join("project.lock"), format!("{seen}|{tracked}"))?;
        Ok(depsmith_core::adapter::Candidate {
            files: vec!["project.lock".into()],
            ..Default::default()
        })
    }
}

#[cfg(unix)]
fn probe(root: &std::path::Path) -> depsmith_core::Result<depsmith_core::Proposal> {
    Engine::new(vec![Box::new(StageProbe)]).prepare(
        root,
        &["probe:project.toml".into()],
        UpdateOptions::default(),
    )
}

#[cfg(unix)]
#[test]
fn symlinks_inside_the_repository_are_staged_as_symlinks() {
    let root = tempdir().unwrap();
    fs::write(root.path().join("project.toml"), "").unwrap();
    fs::write(root.path().join("LICENSE"), "MIT").unwrap();
    fs::create_dir_all(root.path().join("crate")).unwrap();
    std::os::unix::fs::symlink("../LICENSE", root.path().join("crate/LICENSE")).unwrap();
    // A link to a directory inside the repository is staged, not walked.
    std::os::unix::fs::symlink("crate", root.path().join("alias")).unwrap();
    let engine = Engine::new(vec![Box::new(StageProbe)]);
    assert_eq!(engine.discover(root.path()).unwrap().len(), 1);
    let proposal = probe(root.path()).unwrap();
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    assert_eq!(proposal.changes[0].after, "link ../LICENSE MIT|");
}

#[cfg(unix)]
#[test]
fn symlinks_that_leave_the_repository_are_rejected() {
    let outside = tempdir().unwrap();
    fs::write(outside.path().join("secret"), "outside").unwrap();
    for target in [
        outside.path().join("secret"),
        std::path::PathBuf::from("../../outside"),
    ] {
        let root = tempdir().unwrap();
        fs::write(root.path().join("project.toml"), "").unwrap();
        fs::create_dir_all(root.path().join("crate")).unwrap();
        std::os::unix::fs::symlink(&target, root.path().join("crate/LICENSE")).unwrap();
        let error = probe(root.path()).unwrap_err().to_string();
        assert!(error.contains("outside the repository"), "{error}");
    }
}

/// Commit everything under `root` to a new Git repository.
#[cfg(unix)]
fn commit_all(root: &std::path::Path) {
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(["-c", "user.name=T", "-c", "user.email=t@example.invalid"])
            .args(args)
            .current_dir(root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .status()
            .unwrap();
        assert!(status.success());
    };
    git(&["init", "-q"]);
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "initial"]);
}

#[cfg(unix)]
#[test]
fn tracked_symlinks_inside_the_repository_are_staged_as_symlinks() {
    let root = tempdir().unwrap();
    fs::write(root.path().join("project.toml"), "").unwrap();
    fs::write(root.path().join("LICENSE"), "MIT").unwrap();
    fs::create_dir_all(root.path().join("crate")).unwrap();
    std::os::unix::fs::symlink("../LICENSE", root.path().join("crate/LICENSE")).unwrap();
    commit_all(root.path());
    let proposal = probe(root.path()).unwrap();
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    assert_eq!(proposal.changes[0].after, "link ../LICENSE MIT|");
}

#[cfg(unix)]
#[test]
fn tracked_files_in_directories_named_like_build_output_are_staged() {
    let root = tempdir().unwrap();
    fs::write(root.path().join("project.toml"), "").unwrap();
    fs::create_dir_all(root.path().join("pkg/target")).unwrap();
    fs::write(root.path().join("pkg/target/mod.py"), "source").unwrap();
    commit_all(root.path());
    let proposal = probe(root.path()).unwrap();
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    assert_eq!(proposal.changes[0].after, "missing|source");
}

/// Writes the name of the directory it resolves in to `project.lock`.
struct DirectoryName;
impl depsmith_core::adapter::Adapter for DirectoryName {
    fn spec(&self) -> depsmith_core::adapter::AdapterSpec {
        depsmith_core::adapter::AdapterSpec::new("named", &["project.toml"])
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
        let name = root.file_name().unwrap().to_string_lossy();
        fs::write(root.join("project.lock"), name.as_bytes())?;
        Ok(depsmith_core::adapter::Candidate {
            files: vec!["project.lock".into()],
            ..Default::default()
        })
    }
}

#[test]
fn the_stage_has_the_name_of_the_repository_directory() {
    // npm names a lock after the directory of a package.json without a
    // name, and Pixi a workspace without one, so the stage keeps it.
    let parent = tempdir().unwrap();
    let root = parent.path().join("helmet");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("project.toml"), "").unwrap();
    let proposal = Engine::new(vec![Box::new(DirectoryName)])
        .prepare(
            &root,
            &["named:project.toml".into()],
            UpdateOptions::default(),
        )
        .unwrap();
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    assert_eq!(proposal.changes[0].after, "helmet");
}

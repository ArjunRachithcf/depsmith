//! The stage: adapters run in a copy that a package manager cannot tell
//! apart from the repository.
use depsmith_core::{
    adapter::{Adapter, AdapterSpec, Candidate},
    Engine, Error, Result, Target, UpdateOptions,
};
use std::{fs, path::Path, process::Command};

/// Reports that the engine reached it.
struct Probe;

impl Adapter for Probe {
    fn spec(&self) -> AdapterSpec {
        AdapterSpec::new("probe", &["probe.txt"])
    }
    fn detects(&self, _: &Path, _: &str) -> bool {
        true
    }
    fn prepare(&self, stage: &Path, target: &Target, _: &UpdateOptions) -> Result<Candidate> {
        assert!(stage.join(&target.manifest).is_file());
        Err(Error::Invalid("probe reached".into()))
    }
}

/// A tracked symlink leaving the repository is a left-out path (ADR 0007):
/// it fails the target whose directory holds it, and only that one. A
/// tracked link the checkout no longer has is not left out.
#[cfg(unix)]
#[test]
fn missing_tracked_symlinks_fail_only_targets_holding_them() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("shop");
    for dir in ["app", "docs"] {
        fs::create_dir_all(root.join(dir)).unwrap();
        fs::write(root.join(dir).join("probe.txt"), "").unwrap();
    }
    std::os::unix::fs::symlink("/nonexistent/share", root.join("docs/share")).unwrap();
    std::os::unix::fs::symlink("../app/probe.txt", root.join("app/alias")).unwrap();
    git(&root, &["init", "-q"]);
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "links"]);
    assert!(failure(&root, "probe:app/probe.txt").contains("probe reached"));
    assert!(failure(&root, "probe:docs/probe.txt").contains(
        "the target's directory holds paths left out of the stage: \
         docs/share (symlink outside the repository)"
    ));
    fs::remove_file(root.join("app/alias")).unwrap();
    assert!(failure(&root, "probe:app/probe.txt").contains("probe reached"));
}

/// The message of the only failure, for the probe `target` in `root`.
fn failure(root: &Path, target: &str) -> String {
    let proposal = Engine::new(vec![Box::new(Probe)])
        .prepare(root, &[target.into()], UpdateOptions::default())
        .unwrap();
    assert_eq!(proposal.failures.len(), 1, "{:?}", proposal.failures);
    proposal.failures[0].message.clone()
}

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(root)
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

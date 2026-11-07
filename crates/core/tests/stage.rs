//! The stage: adapters run in a copy that a package manager cannot tell
//! apart from the repository.
use depsmith_core::{
    adapter::{Adapter, AdapterSpec, Candidate},
    Engine, Error, Result, Target, UpdateOptions,
};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// Reports the name of the directory it is asked to resolve in.
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
        Err(Error::Invalid(format!(
            "probe reached in {}",
            stage.file_name().unwrap().to_string_lossy()
        )))
    }
}

/// Package managers name a project without a declared name after its
/// directory.
#[test]
fn the_stage_directory_has_the_repository_directory_name() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("shop");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("probe.txt"), "").unwrap();
    assert!(failure(&root, "probe:probe.txt").ends_with("probe reached in shop"));
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

/// What one target saw of the stage: every entry with what it is (a
/// directory, a link and its target, or a file, its execute bit and bytes),
/// including `.git/config`, `.git/hooks` and what sits beside the stage as
/// `../name`; and the inode of `docs/guide.md`, which no target touches.
type Seen = (BTreeMap<PathBuf, String>, u64);

/// Records the stage, then leaves it as a package manager might: an ignored
/// install directory, an edited, an executable, a removed and an added file,
/// an empty directory, a link replaced by a file, a Git hook, an edited Git
/// config and index, a file beside the stage and a read-only directory of
/// inputs; with `read_only`, also a directory staging cannot delete.
struct Messy {
    seen: Arc<Mutex<Vec<Seen>>>,
    // Read-only directories are a Unix case.
    #[cfg_attr(not(unix), allow(dead_code))]
    read_only: bool,
}

impl Adapter for Messy {
    fn spec(&self) -> AdapterSpec {
        AdapterSpec::new("messy", &["probe.txt"])
    }
    fn detects(&self, _: &Path, _: &str) -> bool {
        true
    }
    fn prepare(&self, stage: &Path, _: &Target, _: &UpdateOptions) -> Result<Candidate> {
        let mut entries = BTreeMap::new();
        list(stage, Path::new(""), &mut entries);
        for entry in fs::read_dir(stage.parent().unwrap())? {
            let name = entry?.file_name();
            if name != stage.file_name().unwrap() {
                entries.insert(Path::new("..").join(name), String::new());
            }
        }
        #[cfg(unix)]
        let inode =
            std::os::unix::fs::MetadataExt::ino(&fs::metadata(stage.join("docs/guide.md"))?);
        #[cfg(not(unix))]
        let inode = 0;
        self.seen.lock().unwrap().push((entries, inode));
        fs::create_dir_all(stage.join("node_modules/left"))?;
        fs::write(stage.join("node_modules/left/index.js"), "")?;
        fs::write(stage.join("README"), "changed")?;
        fs::remove_file(stage.join("docs/old.md"))?;
        fs::write(stage.join("stray.txt"), "")?;
        fs::create_dir(stage.join("empty"))?;
        fs::write(stage.join(".git/index"), "not an index")?;
        fs::create_dir_all(stage.join(".git/hooks"))?;
        fs::write(stage.join(".git/hooks/post-checkout"), "#!/bin/sh\n")?;
        let config = fs::read_to_string(stage.join(".git/config"))?;
        fs::write(
            stage.join(".git/config"),
            config + "[core]\n\thooksPath = /tmp\n",
        )?;
        fs::write(stage.parent().unwrap().join("beside.txt"), "")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(stage.join("README"), fs::Permissions::from_mode(0o755))?;
            fs::remove_file(stage.join("alias"))?;
            fs::write(stage.join("alias"), "not a link")?;
            fs::set_permissions(stage.join("docs"), fs::Permissions::from_mode(0o555))?;
            if self.read_only {
                fs::create_dir_all(stage.join("cache/module"))?;
                fs::write(stage.join("cache/module/go.mod"), "")?;
                fs::set_permissions(
                    stage.join("cache/module"),
                    fs::Permissions::from_mode(0o555),
                )?;
            }
        }
        Err(Error::Invalid("messy reached".into()))
    }
}

/// Every entry under `dir` of `root`, described as in [`Seen`]; of `.git`,
/// only its config and hooks.
fn list(root: &Path, dir: &Path, entries: &mut BTreeMap<PathBuf, String>) {
    for entry in fs::read_dir(root.join(dir)).unwrap() {
        let path = dir.join(entry.unwrap().file_name());
        let full = root.join(&path);
        let meta = fs::symlink_metadata(&full).unwrap();
        let what = if meta.file_type().is_symlink() {
            format!("link {}", fs::read_link(&full).unwrap().display())
        } else if meta.is_dir() {
            if path == Path::new(".git") {
                let config = fs::read_to_string(full.join("config")).unwrap();
                entries.insert(path.join("config"), config);
                if full.join("hooks").exists() {
                    list(root, &path.join("hooks"), entries);
                }
                continue;
            }
            list(root, &path, entries);
            "directory".into()
        } else {
            #[cfg(unix)]
            let mode = std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o111;
            #[cfg(not(unix))]
            let mode = 0;
            format!(
                "file {mode:o} {}",
                String::from_utf8_lossy(&fs::read(&full).unwrap())
            )
        };
        entries.insert(path, what);
    }
}

/// Two messy targets in one Git repository; what each saw of the stage.
fn messy_run(read_only: bool) -> Vec<Seen> {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("shop");
    for dir in ["a", "b", "docs"] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    for file in ["a/probe.txt", "b/probe.txt", "docs/guide.md", "docs/old.md"] {
        fs::write(root.join(file), file).unwrap();
    }
    fs::write(root.join("README"), "readme").unwrap();
    fs::write(root.join(".gitignore"), "node_modules/\n").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("README", root.join("alias")).unwrap();
    git(&root, &["init", "-q"]);
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "shop"]);
    let seen = Arc::new(Mutex::new(vec![]));
    let proposal = Engine::new(vec![Box::new(Messy {
        seen: seen.clone(),
        read_only,
    })])
    .prepare(
        &root,
        &["messy:a/probe.txt".into(), "messy:b/probe.txt".into()],
        UpdateOptions::default(),
    )
    .unwrap();
    assert_eq!(proposal.failures.len(), 2, "{:?}", proposal.failures);
    assert!(proposal
        .failures
        .iter()
        .all(|f| f.message.contains("messy reached")));
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    seen
}

/// Each target resolves in a stage that matches the repository, whatever
/// the target before it left behind.
#[test]
fn each_target_sees_the_repository_not_what_the_last_target_left() {
    let seen = messy_run(false);
    let mut names = vec![
        ".git/config",
        ".gitignore",
        "README",
        "a",
        "a/probe.txt",
        "b",
        "b/probe.txt",
        "docs",
        "docs/guide.md",
        "docs/old.md",
    ];
    if cfg!(unix) {
        names.insert(5, "alias");
    }
    let listed: Vec<_> = seen[0].0.keys().map(PathBuf::as_path).collect();
    assert_eq!(listed, names.iter().map(Path::new).collect::<Vec<_>>());
    assert_eq!(seen[0].0, seen[1].0);
}

/// A run stages the repository once: a file no target touched is not
/// copied again for the next target.
#[cfg(unix)]
#[test]
fn an_untouched_file_is_staged_once_per_run() {
    let seen = messy_run(false);
    assert_eq!(seen[0].1, seen[1].1);
}

/// A target can leave what staging cannot delete, such as a read-only
/// module cache: the next target is staged afresh instead of failing the run.
#[cfg(unix)]
#[test]
fn a_leftover_staging_cannot_delete_does_not_stop_the_run() {
    let seen = messy_run(true);
    assert_eq!(seen[0].0, seen[1].0);
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
    let status = std::process::Command::new("git")
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

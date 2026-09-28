use crate::{Error, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
};

pub(crate) fn relative(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(Error::Invalid(format!(
            "expected a relative path inside workspace: {}",
            path.display()
        )));
    }
    Ok(())
}

pub(crate) fn files(root: &Path) -> Result<Vec<PathBuf>> {
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .filter_entry(|e| {
            e.depth() == 0
                || !matches!(
                    e.file_name().to_str(),
                    Some(
                        ".pixi"
                            | ".git"
                            | ".venv"
                            | "node_modules"
                            | "target"
                            | ".depsmith"
                            | "__pycache__"
                    )
                )
        })
        .build();
    let mut paths = vec![];
    for entry in walker {
        let entry = entry.map_err(|e| Error::Operation(e.to_string()))?;
        if entry.depth() == 0 {
            continue;
        }
        if entry.file_type().is_some_and(|t| t.is_dir()) && entry.path().join(".git").exists() {
            return Err(Error::Invalid(
                "nested Git repositories are not supported for isolated staging".into(),
            ));
        }
        let path = entry
            .path()
            .strip_prefix(root)
            .map_err(|e| Error::Operation(e.to_string()))?;
        if entry.file_type().is_some_and(|t| t.is_symlink()) {
            return Err(Error::Invalid(format!(
                "symlink requires an explicit staging policy: {}",
                path.display()
            )));
        }
        if entry.file_type().is_some_and(|t| t.is_file()) {
            paths.push(path.to_path_buf());
        }
    }
    // Git can track files that now match ignore rules. Omitting those files
    // changes SCM dirty-state detection (and can drop required build sources).
    if let Some(snapshot) = crate::scm::Snapshot::read(root)? {
        for path in snapshot.tracked(root)? {
            if path.components().any(|c| {
                matches!(
                    c.as_os_str().to_str(),
                    Some(
                        ".pixi" | ".venv" | "node_modules" | "target" | ".depsmith" | "__pycache__"
                    )
                )
            }) {
                return Err(Error::Invalid(format!(
                    "tracked file uses an excluded staging directory: {}",
                    path.display()
                )));
            }
            paths.push(path);
        }
    }
    // Native configuration is a resolver input even when .pixi is gitignored.
    // Probe only manifest directories; never traverse installed environments.
    let manifests = paths.clone();
    for manifest in manifests {
        if matches!(
            manifest.file_name().and_then(|n| n.to_str()),
            Some("pixi.toml" | "pyproject.toml")
        ) {
            for input in [".pixi/config.toml", "pixi.lock"] {
                let path = manifest.parent().unwrap().join(input);
                output_path(root, &path)?;
                if root.join(&path).is_file() {
                    paths.push(path);
                }
            }
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

pub(crate) fn fingerprint(root: &Path) -> Result<BTreeMap<PathBuf, String>> {
    let mut inputs: BTreeMap<_, _> = files(root)?
        .into_iter()
        .map(|path| {
            let hash = file_hash(&root.join(&path))?;
            Ok((path, hash))
        })
        .collect::<Result<_>>()?;
    if let Some(snapshot) = crate::scm::Snapshot::read(root)? {
        for (relative, source) in snapshot.files {
            inputs.insert(relative, file_hash(&source)?);
        }
        inputs.insert(
            ".git/config".into(),
            format!("{:x}", Sha256::digest(snapshot.config.as_bytes())),
        );
    }
    Ok(inputs)
}

fn file_hash(path: &Path) -> Result<String> {
    let mut hash = Sha256::new();
    std::io::copy(&mut fs::File::open(path)?, &mut hash)?;
    #[cfg(unix)]
    let suffix = {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(path)?.permissions().mode() & 0o111 != 0 {
            ":executable"
        } else {
            ""
        }
    };
    #[cfg(not(unix))]
    let suffix = "";
    Ok(format!("{:x}{suffix}", hash.finalize()))
}

pub(crate) fn stage(
    root: &Path,
    destination: &Path,
    inputs: &BTreeMap<PathBuf, String>,
) -> Result<()> {
    for path in inputs.keys() {
        if path.starts_with(".git") {
            continue;
        }
        let dest = destination.join(path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(root.join(path), dest)?;
    }
    if let Some(snapshot) = crate::scm::Snapshot::read(root)? {
        snapshot.copy(destination)?;
    }
    if fingerprint(destination)? != *inputs {
        return Err(Error::Stale("inputs changed during staging".into()));
    }
    Ok(())
}

/// Validate output ownership before reading or replacing any candidate bytes.
pub(crate) fn output_path(root: &Path, path: &Path) -> Result<()> {
    relative(path)?;
    if path
        .components()
        .any(|part| matches!(part.as_os_str().to_str(), Some(".git" | ".depsmith")))
    {
        return Err(Error::Invalid("output targets internal metadata".into()));
    }
    let mut current = root.to_path_buf();
    for part in path.components() {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(Error::Invalid("symlink in output path".into()))
            }
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(root: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
            ])
            .args(args)
            .current_dir(root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .env("GIT_OPTIONAL_LOCKS", "0")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn git_project() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "--initial-branch=main"]);
        fs::write(root.path().join("pixi.toml"), "[workspace]\nname='demo'\n").unwrap();
        fs::write(root.path().join("tracked.txt"), "original\n").unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-m", "initial"]);
        git(root.path(), &["tag", "v1.2.3"]);
        root
    }

    #[test]
    fn git_stage_preserves_describe_dirty_and_ignored_tracked_files_without_config_or_hooks() {
        let root = git_project();
        fs::write(root.path().join(".gitignore"), "tracked.txt\n").unwrap();
        fs::write(root.path().join("tracked.txt"), "user edit\n").unwrap();
        git(
            root.path(),
            &[
                "config",
                "remote.origin.url",
                "https://secret@example.invalid/repo",
            ],
        );
        fs::write(root.path().join(".git/hooks/private-hook"), "secret hook").unwrap();
        let index = fs::read(root.path().join(".git/index")).unwrap();
        let destination = tempfile::tempdir().unwrap();
        let inputs = fingerprint(root.path()).unwrap();
        stage(root.path(), destination.path(), &inputs).unwrap();
        assert!(destination.path().join(".git/HEAD").exists());
        assert_eq!(
            fs::read_to_string(destination.path().join("tracked.txt")).unwrap(),
            "user edit\n"
        );
        assert_eq!(
            git(root.path(), &["describe", "--tags", "--dirty"]),
            git(destination.path(), &["describe", "--tags", "--dirty"])
        );
        assert!(!fs::read_to_string(destination.path().join(".git/config"))
            .unwrap()
            .contains("secret"));
        assert!(!destination.path().join(".git/hooks/private-hook").exists());
        assert_eq!(fs::read(root.path().join(".git/index")).unwrap(), index);
    }

    #[test]
    fn git_tags_and_index_changes_invalidate_fingerprint() {
        let root = git_project();
        let before = fingerprint(root.path()).unwrap();
        git(root.path(), &["tag", "v2.0.0"]);
        assert_ne!(before, fingerprint(root.path()).unwrap());
        let before = fingerprint(root.path()).unwrap();
        fs::write(root.path().join("tracked.txt"), "staged\n").unwrap();
        git(root.path(), &["add", "tracked.txt"]);
        fs::write(root.path().join("tracked.txt"), "original\n").unwrap();
        assert_ne!(before, fingerprint(root.path()).unwrap());
    }

    #[test]
    fn linked_git_worktree_becomes_self_contained_stage() {
        let root = git_project();
        let parent = tempfile::tempdir().unwrap();
        let worktree = parent.path().join("linked");
        git(
            root.path(),
            &["worktree", "add", "--detach", worktree.to_str().unwrap()],
        );
        let destination = tempfile::tempdir().unwrap();
        stage(
            &worktree,
            destination.path(),
            &fingerprint(&worktree).unwrap(),
        )
        .unwrap();
        assert!(destination.path().join(".git").is_dir());
        assert!(!destination.path().join(".git/commondir").exists());
        assert_eq!(
            git(&worktree, &["describe", "--tags", "--dirty"]),
            git(destination.path(), &["describe", "--tags", "--dirty"])
        );
    }

    #[test]
    fn external_git_attributes_are_rejected_instead_of_changing_dirty_state() {
        let root = git_project();
        let external = tempfile::NamedTempFile::new().unwrap();
        fs::write(external.path(), "*.txt text\n").unwrap();
        git(root.path(), &["config", "core.autocrlf", "false"]);
        git(
            root.path(),
            &[
                "config",
                "core.attributesFile",
                external.path().to_str().unwrap(),
            ],
        );
        fs::write(root.path().join("tracked.txt"), "original\r\n").unwrap();
        git(root.path(), &["add", "tracked.txt"]);
        assert_eq!(
            git(root.path(), &["describe", "--tags", "--dirty"]).trim(),
            "v1.2.3"
        );
        assert!(matches!(fingerprint(root.path()), Err(Error::Invalid(_))));
    }

    #[test]
    fn nested_and_ancestor_git_repositories_are_rejected_explicitly() {
        let root = git_project();
        let child = root.path().join("nested");
        fs::create_dir(&child).unwrap();
        fs::write(child.join("pixi.toml"), "[workspace]\nname='nested'\n").unwrap();
        assert!(matches!(fingerprint(&child), Err(Error::Invalid(_))));
        git(&child, &["init"]);
        assert!(matches!(fingerprint(root.path()), Err(Error::Invalid(_))));
    }

    #[cfg(unix)]
    #[test]
    fn git_metadata_symlinks_and_external_objects_are_rejected() {
        let root = git_project();
        fs::write(
            root.path().join(".git/objects/info/alternates"),
            "/external/objects\n",
        )
        .unwrap();
        assert!(matches!(fingerprint(root.path()), Err(Error::Invalid(_))));
        fs::remove_file(root.path().join(".git/objects/info/alternates")).unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("exclude"), "secret\n").unwrap();
        fs::remove_dir_all(root.path().join(".git/info")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join(".git/info")).unwrap();
        assert!(matches!(fingerprint(root.path()), Err(Error::Invalid(_))));
    }
    #[test]
    fn ignored_existing_lock_is_always_staged_and_fingerprinted() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        fs::write(source.path().join(".gitignore"), "pixi.lock\n").unwrap();
        fs::write(
            source.path().join("pixi.toml"),
            "[workspace]\nname='demo'\n",
        )
        .unwrap();
        fs::write(source.path().join("pixi.lock"), "existing baseline").unwrap();
        let inputs = fingerprint(source.path()).unwrap();
        assert!(inputs.contains_key(Path::new("pixi.lock")));
        stage(source.path(), destination.path(), &inputs).unwrap();
        assert_eq!(
            fs::read_to_string(destination.path().join("pixi.lock")).unwrap(),
            "existing baseline"
        );
        fs::write(source.path().join("pixi.lock"), "changed baseline").unwrap();
        assert_ne!(inputs, fingerprint(source.path()).unwrap());
    }

    #[test]
    fn ignored_native_pixi_configuration_is_staged_and_fingerprinted() {
        let source = tempfile::tempdir().unwrap();
        let stage_dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(source.path().join(".pixi/envs/default")).unwrap();
        fs::write(source.path().join(".gitignore"), ".pixi/\n").unwrap();
        fs::write(
            source.path().join("pixi.toml"),
            "[workspace]\nname='demo'\n",
        )
        .unwrap();
        fs::write(
            source.path().join(".pixi/config.toml"),
            "[repodata-config]\n",
        )
        .unwrap();
        fs::write(
            source.path().join(".pixi/envs/default/installed"),
            "ignored",
        )
        .unwrap();
        let inputs = fingerprint(source.path()).unwrap();
        assert!(inputs.contains_key(Path::new(".pixi/config.toml")));
        stage(source.path(), stage_dir.path(), &inputs).unwrap();
        assert!(!stage_dir.path().join(".pixi/envs").exists());
        fs::write(source.path().join(".pixi/config.toml"), "changed").unwrap();
        assert_ne!(inputs, fingerprint(source.path()).unwrap());
    }
}

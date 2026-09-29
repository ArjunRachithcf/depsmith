//! Self-contained, data-only Git staging. Never copy repository configuration,
//! hooks, credentials, worktree back-pointers, or external object-store links.
use crate::{Error, Result};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

pub(crate) struct Snapshot {
    git_dir: PathBuf,
    pub files: BTreeMap<PathBuf, PathBuf>,
    pub config: String,
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let mut command = Command::new("git");
    // Repository/index overrides inherited from a caller must not redirect reads.
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["-c", "core.fsmonitor=false"])
        .args(args);
    crate::process::run_command(command, "git", root, 60)
}

fn no_symlinks(path: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    for part in path.components() {
        current.push(part);
        // A drive prefix or root cannot be a symlink, and Windows rejects
        // metadata queries on a bare volume such as `\\?\C:`.
        if !matches!(part, std::path::Component::Normal(_)) {
            continue;
        }
        if fs::symlink_metadata(&current)?.file_type().is_symlink() {
            return Err(Error::Invalid(
                "symlink in Git metadata is unsupported".into(),
            ));
        }
    }
    Ok(())
}

impl Snapshot {
    pub fn read(root: &Path) -> Result<Option<Self>> {
        Self::read_canonical(&crate::working_tree::canonical(root)?)
    }

    fn read_canonical(root: &Path) -> Result<Option<Self>> {
        let marker = root.join(".git");
        if !marker.try_exists()? {
            if root.ancestors().skip(1).any(|parent| {
                let marker = parent.join(".git");
                marker.is_file() || marker.join("HEAD").exists()
            }) {
                return Err(Error::Invalid("the selected root is inside a Git repository; select the repository root for SCM staging".into()));
            }
            return Ok(None);
        }
        no_symlinks(&marker)?;
        let git_dir = if marker.is_file() {
            let text = fs::read_to_string(&marker)?;
            let path = text
                .trim()
                .strip_prefix("gitdir: ")
                .ok_or_else(|| Error::Invalid("invalid Git worktree pointer".into()))?;
            let path = root.join(path);
            no_symlinks(&path)?;
            crate::working_tree::canonical(&path)?
        } else {
            marker
        };
        // Some managed development environments expose an empty .git mount.
        if !git_dir.join("HEAD").try_exists()? {
            return Ok(None);
        }
        let common_file = git_dir.join("commondir");
        let common = if common_file.try_exists()? {
            no_symlinks(&common_file)?;
            let path = git_dir.join(fs::read_to_string(&common_file)?.trim());
            no_symlinks(&path)?;
            crate::working_tree::canonical(&path)?
        } else {
            git_dir.clone()
        };
        let config = sanitized_config(root, &common.join("config"))?;
        // Worktree-specific config may change sparse checkout or object semantics.
        if git_dir.join("config.worktree").try_exists()? {
            return Err(Error::Invalid(
                "Git worktree-specific configuration is not supported for staging".into(),
            ));
        }
        for unsupported in [
            "objects/info/alternates",
            "objects/info/http-alternates",
            "info/grafts",
            "info/sparse-checkout",
            "reftable",
        ] {
            if common.join(unsupported).try_exists()? || git_dir.join(unsupported).try_exists()? {
                return Err(Error::Invalid(format!(
                    "Git metadata {unsupported} is not supported for isolated staging"
                )));
            }
        }
        let mut snapshot = Self {
            git_dir: git_dir.clone(),
            files: BTreeMap::new(),
            config,
        };
        for name in ["HEAD", "index"] {
            snapshot.add(&git_dir.join(name), &Path::new(".git").join(name))?;
        }
        for entry in fs::read_dir(&git_dir)? {
            let entry = entry?;
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with("sharedindex.")
            {
                snapshot.add(&entry.path(), &Path::new(".git").join(entry.file_name()))?;
            }
        }
        for name in [
            "objects",
            "refs",
            "packed-refs",
            "shallow",
            "info/exclude",
            "info/attributes",
        ] {
            snapshot.add(&common.join(name), &Path::new(".git").join(name))?;
        }
        Ok(Some(snapshot))
    }

    fn add(&mut self, source: &Path, destination: &Path) -> Result<()> {
        match fs::symlink_metadata(source) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(Error::Invalid(
                    "symlink in Git metadata is unsupported".into(),
                ))
            }
            Ok(meta) if meta.is_dir() => {
                for entry in fs::read_dir(source)? {
                    let entry = entry?;
                    self.add(&entry.path(), &destination.join(entry.file_name()))?;
                }
            }
            Ok(meta) if meta.is_file() => {
                no_symlinks(source)?;
                self.files.insert(destination.into(), source.into());
            }
            Ok(_) => return Err(Error::Invalid("special file in Git metadata".into())),
        }
        Ok(())
    }

    pub fn tracked(&self, root: &Path) -> Result<Vec<PathBuf>> {
        let output = git(
            root,
            &[
                "--git-dir",
                self.git_dir
                    .to_str()
                    .ok_or_else(|| Error::Invalid("Git directory must be UTF-8".into()))?,
                "--work-tree",
                root.to_str()
                    .ok_or_else(|| Error::Invalid("repository root must be UTF-8".into()))?,
                "ls-files",
                "--cached",
                "--stage",
                "-z",
            ],
        )?;
        let mut paths = vec![];
        for record in output.split('\0').filter(|s| !s.is_empty()) {
            let (entry, path) = record
                .split_once('\t')
                .ok_or_else(|| Error::Operation("invalid Git index listing".into()))?;
            if entry.starts_with("160000 ") {
                return Err(Error::Invalid(
                    "Git submodules are not supported for isolated staging".into(),
                ));
            }
            let path = PathBuf::from(path);
            crate::working_tree::relative(&path)?;
            crate::working_tree::output_path(root, &path)?;
            if root.join(&path).try_exists()? {
                paths.push(path);
            }
        }
        Ok(paths)
    }

    pub fn copy(&self, destination: &Path) -> Result<()> {
        fs::create_dir_all(destination.join(".git/objects"))?;
        fs::create_dir_all(destination.join(".git/refs"))?;
        for (relative, source) in &self.files {
            no_symlinks(source)?;
            let dest = destination.join(relative);
            fs::create_dir_all(dest.parent().unwrap())?;
            fs::copy(source, dest)?;
        }
        fs::write(destination.join(".git/config"), &self.config)?;
        Ok(())
    }
}

fn sanitized_config(root: &Path, config: &Path) -> Result<String> {
    no_symlinks(config)?;
    let output = git(
        root,
        &[
            "config",
            "--file",
            config
                .to_str()
                .ok_or_else(|| Error::Invalid("Git config path must be UTF-8".into()))?,
            "--no-includes",
            "--null",
            "--list",
        ],
    )?;
    let mut values = BTreeMap::new();
    for record in output.split('\0').filter(|s| !s.is_empty()) {
        let (key, value) = record.split_once('\n').unwrap_or((record, "true"));
        let allowed = match key {
            "core.repositoryformatversion" => Some(&["0", "1"][..]),
            "core.filemode" | "core.ignorecase" | "core.symlinks" => Some(&["true", "false"][..]),
            "core.autocrlf" => Some(&["true", "false", "input"][..]),
            "core.eol" => Some(&["lf", "crlf", "native"][..]),
            "extensions.objectformat" => Some(&["sha1", "sha256"][..]),
            "extensions.worktreeconfig" => continue,
            k if k.starts_with("extensions.")
                || k.starts_with("include.")
                || k.starts_with("includeif.")
                || k.starts_with("filter.")
                || matches!(k, "core.sparsecheckout" | "core.attributesfile") =>
            {
                return Err(Error::Invalid(format!(
                    "Git setting {key} is not supported for isolated staging"
                )));
            }
            _ => None,
        };
        if let Some(allowed) = allowed {
            let value = value.to_ascii_lowercase();
            if !allowed.contains(&value.as_str()) {
                return Err(Error::Invalid(format!(
                    "unsupported value for Git setting {key}"
                )));
            }
            values.insert(key, value);
        }
    }
    let mut result = "[core]\n\tbare = false\n".to_owned();
    for (key, value) in values {
        let (section, name) = key.split_once('.').unwrap();
        result.push_str(&format!("[{section}]\n\t{name} = {value}\n"));
    }
    Ok(result)
}

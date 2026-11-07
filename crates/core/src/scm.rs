//! Self-contained, data-only Git staging. Never copy repository configuration,
//! hooks, credentials, worktree back-pointers, or external object-store links.
use crate::{Error, Result};
use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::Command,
};

pub(crate) struct Snapshot {
    git_dir: PathBuf,
    pub files: BTreeMap<PathBuf, PathBuf>,
    pub config: String,
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    crate::process::run_command(git_command(args), "git", root, 60)
}

/// `git args`, with no inherited repository overrides, system or global
/// configuration, prompts or optional locks.
fn git_command(args: &[&str]) -> Command {
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
    command
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

/// What the index tracks, by kind.
#[derive(Debug, Default)]
pub(crate) struct Tracked {
    pub(crate) files: Vec<PathBuf>,
    pub(crate) links: Vec<PathBuf>,
    pub(crate) submodules: Vec<PathBuf>,
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

    /// The tracked files, symlinks and submodules; staging decides what
    /// links and submodules become (ADR 0007).
    pub fn tracked(&self, root: &Path) -> Result<Tracked> {
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
        let mut tracked = Tracked::default();
        for record in output.split('\0').filter(|s| !s.is_empty()) {
            let (entry, path) = record
                .split_once('\t')
                .ok_or_else(|| Error::Operation("invalid Git index listing".into()))?;
            if entry.starts_with("160000 ") {
                tracked.submodules.push(PathBuf::from(path));
                continue;
            }
            let path = PathBuf::from(path);
            // The index can list a link the checkout no longer has, or a
            // left-out path (ADR 0007) when `root` is the stage. With
            // core.symlinks=false the link is checked out as a file.
            if entry.starts_with("120000 ")
                && fs::symlink_metadata(root.join(&path)).is_ok_and(|m| m.is_symlink())
            {
                tracked.links.push(path);
                continue;
            }
            crate::working_tree::relative(&path)?;
            crate::working_tree::output_path(root, &path)?;
            if root.join(&path).try_exists()? {
                tracked.files.push(path);
            }
        }
        Ok(tracked)
    }

    /// Marks the index entries `destination` lacks (left-out paths, ADR
    /// 0007) as unchanged, so Git in the stage reports the repository's
    /// dirty state instead of those paths as deleted.
    pub(crate) fn keep_missing_unchanged(destination: &Path) -> Result<()> {
        let missing = git(destination, &["ls-files", "--deleted", "-z"])?;
        let missing: Vec<&str> = missing.split('\0').filter(|p| !p.is_empty()).collect();
        for paths in missing.chunks(100) {
            let mut args = vec!["update-index", "--assume-unchanged", "--"];
            args.extend(paths);
            git(destination, &args)?;
        }
        Ok(())
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

/// The `REVISION:./path` spec git reads a file's committed content from, or
/// its staged content when `revision` is `None`.
fn object_spec(revision: Option<&str>, path: &Path) -> Result<String> {
    let mut relative = String::from(".");
    for part in path.components() {
        let std::path::Component::Normal(part) = part else {
            return Err(Error::Invalid(format!(
                "{} must be a relative path inside the repository",
                path.display()
            )));
        };
        relative.push('/');
        relative.push_str(
            part.to_str()
                .ok_or_else(|| Error::Invalid("paths read from Git must be UTF-8".into()))?,
        );
    }
    Ok(format!("{}:{relative}", revision.unwrap_or_default()))
}

/// `git args` in `root`, with a failure reported as an invalid request.
fn git_or_invalid(root: &Path, args: &[&str]) -> Result<String> {
    git(root, args).map_err(|e| Error::Invalid(format!("git {}: {e}", args.join(" "))))
}

/// Fails unless `root` is in a git repository.
fn in_repository(root: &Path) -> Result<()> {
    git_or_invalid(root, &["rev-parse", "--show-toplevel"]).map(|_| ())
}

/// The commit `revision` names, in the repository `root` is in.
fn resolve(root: &Path, revision: &str) -> Result<String> {
    let spec = format!("{revision}^{{commit}}");
    git(root, &["rev-parse", "--verify", "--quiet", &spec])
        .map(|sha| sha.trim().to_owned())
        .map_err(|_| Error::Invalid(format!("unknown Git revision {revision}")))
}

/// The merge-base of `HEAD` with the default branch: `origin/HEAD`, else
/// `origin/main`, else `main`.
pub(crate) fn merge_base(root: &Path) -> Result<String> {
    in_repository(root)?;
    let branch = ["origin/HEAD", "origin/main", "main"]
        .into_iter()
        .find(|branch| resolve(root, branch).is_ok())
        .ok_or_else(|| {
            Error::Invalid(
                "no default branch to compare with (tried origin/HEAD, origin/main and main); pass --base".into(),
            )
        })?;
    Ok(git_or_invalid(root, &["merge-base", "HEAD", branch])?
        .trim()
        .to_owned())
}

/// Write each of `paths` (relative to `root`, the repository or a directory
/// in it) as it is at `revision`, or in the index when `None`, under
/// `destination`. A path missing there is left out.
pub(crate) fn materialize(
    root: &Path,
    revision: Option<&str>,
    paths: &[PathBuf],
    destination: &Path,
) -> Result<()> {
    in_repository(root)?;
    let revision = revision.map(|r| resolve(root, r)).transpose()?;
    for path in paths {
        let object = object_spec(revision.as_deref(), path)?;
        if git(root, &["cat-file", "-e", &object]).is_err() {
            continue;
        }
        write_under(destination, path, git_or_invalid(root, &["show", &object])?)?;
    }
    Ok(())
}

/// Write every regular file under `dir` (relative to `root`, the
/// repository or a directory in it) as it is at `revision`, or in the index
/// when `None`, to the same path under `destination`, with the ignore files
/// of the directories above it. Files are written exactly as git stores
/// them: no attributes, filters or line-ending conversion apply, and
/// symlinks, submodules and unmerged entries are left out.
pub(crate) fn materialize_tree(
    root: &Path,
    revision: Option<&str>,
    dir: &Path,
    destination: &Path,
) -> Result<()> {
    in_repository(root)?;
    let ignores: Vec<PathBuf> = dir
        .ancestors()
        .skip(1)
        .map(|parent| parent.join(".gitignore"))
        .collect();
    materialize(root, revision, &ignores, destination)?;
    // Run in `dir`, both listings hold only that directory, with paths
    // relative to it.
    let cwd = root.join(dir);
    let listing = match revision {
        Some(revision) => {
            let commit = resolve(root, revision)?;
            git_or_invalid(&cwd, &["ls-tree", "-r", "-z", &commit])?
        }
        None => git_or_invalid(&cwd, &["ls-files", "--stage", "-z"])?,
    };
    let mut blobs = vec![];
    for record in listing.split('\0').filter(|r| !r.is_empty()) {
        let Some((meta, path)) = record.split_once('\t') else {
            continue;
        };
        // `<mode> blob <object>` from ls-tree, `<mode> <object> <stage>` from
        // ls-files.
        let fields: Vec<&str> = meta.split(' ').collect();
        let (mode, object) = match (revision, fields.as_slice()) {
            (Some(_), [mode, "blob", object]) | (None, [mode, object, "0"]) => (*mode, *object),
            _ => continue,
        };
        if matches!(mode, "100644" | "100755") {
            blobs.push((object.to_owned(), dir.join(path)));
        }
    }
    let mut input = tempfile::tempfile()?;
    for (object, _) in &blobs {
        writeln!(input, "{object}")?;
    }
    input.seek(SeekFrom::Start(0))?;
    let output = crate::process::run_command_with_input(
        git_command(&["cat-file", "--batch"]),
        "git",
        root,
        600,
        input,
    )?;
    let mut output = BufReader::new(output);
    let unreadable = || Error::Invalid("git cat-file returned an unexpected object".into());
    for (object, path) in blobs {
        let mut header = String::new();
        output.read_line(&mut header)?;
        // `<object> blob <size>`
        let size = match header.trim_end().split(' ').collect::<Vec<_>>().as_slice() {
            [found, "blob", size] if *found == object => size.parse().map_err(|_| unreadable())?,
            _ => return Err(unreadable()),
        };
        let mut contents = vec![0; size];
        output.read_exact(&mut contents)?;
        output.read_exact(&mut [0; 1])?;
        if path
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
        {
            write_under(destination, &path, contents)?;
        }
    }
    Ok(())
}

/// Write `contents` to `path` under `destination`, creating its directories.
pub(crate) fn write_under(
    destination: &Path,
    path: &Path,
    contents: impl AsRef<[u8]>,
) -> Result<()> {
    let file = destination.join(path);
    fs::create_dir_all(file.parent().unwrap_or(destination))?;
    Ok(fs::write(file, contents)?)
}

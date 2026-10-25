//! The repository's working tree: listing files while respecting ignore
//! rules, fingerprinting inputs, copying them into a stage, and rejecting
//! paths that escape the repository.
use crate::{adapter::AdapterSpec, Error, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

/// Whether `path` is non-empty and made only of normal components, so that
/// joining it to a directory stays inside that directory.
pub(crate) fn contained(path: &Path) -> bool {
    !path.as_os_str().is_empty() && path.components().all(|c| matches!(c, Component::Normal(_)))
}

pub(crate) fn relative(path: &Path) -> Result<()> {
    if !contained(path) {
        return Err(Error::Invalid(format!(
            "expected a relative path inside the repository: {}",
            path.display()
        )));
    }
    Ok(())
}

/// Directories that never hold project sources, whatever the package manager.
const ALWAYS_SKIPPED: &[&str] = &[
    ".depsmith",
    ".venv",
    "node_modules",
    "target",
    "__pycache__",
];

/// How the working tree is walked for a set of adapters: directories never
/// entered, and ignored files that are still inputs of their targets.
#[derive(Debug, Clone, Default)]
pub(crate) struct Layout {
    skip_dirs: BTreeSet<String>,
    managed: Vec<crate::adapter::ManagedFiles>,
}

impl Layout {
    pub(crate) fn new<'a>(specs: impl IntoIterator<Item = &'a AdapterSpec>) -> Self {
        let mut layout = Self {
            skip_dirs: ALWAYS_SKIPPED.iter().map(|d| d.to_string()).collect(),
            managed: vec![],
        };
        for spec in specs {
            layout.skip_dirs.extend(spec.skip_dirs.iter().cloned());
            layout.managed.extend(spec.managed.iter().cloned());
        }
        layout
    }
}

/// Whether `name` matches a file-name `pattern`, where `*` matches any run of
/// characters.
pub(crate) fn matches_pattern(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == name,
        Some((prefix, rest)) => {
            let Some(mut tail) = name.strip_prefix(prefix) else {
                return false;
            };
            if rest.is_empty() {
                return true;
            }
            loop {
                if matches_pattern(rest, tail) {
                    return true;
                }
                let mut chars = tail.chars();
                if chars.next().is_none() {
                    return false;
                }
                tail = chars.as_str();
            }
        }
    }
}

/// Why staging leaves a path out (ADR 0007).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reason {
    /// A symlink that resolves outside the repository.
    OutsideLink,
    /// A symlink that resolves into another left-out path.
    IntoLeftOut,
    /// A symlink whose target is not UTF-8 text.
    UnreadableLink,
    /// A symlink on a platform where depsmith cannot create one.
    NoSymlinks,
    /// A nested Git repository or a submodule.
    Nested,
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Reason::OutsideLink => "symlink outside the repository",
            Reason::IntoLeftOut => "symlink into a left-out path",
            Reason::UnreadableLink => "symlink target is not UTF-8",
            Reason::NoSymlinks => "symlinks cannot be created on this platform",
            Reason::Nested => "nested Git repository or submodule",
        })
    }
}

/// A path the stage does not copy, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LeftOut {
    pub(crate) path: PathBuf,
    pub(crate) reason: Reason,
}

/// The working tree as staging sees it: regular files, in-repository
/// symlinks (path and target text) and left-out paths.
#[derive(Debug, Default)]
pub(crate) struct Tree {
    pub(crate) files: Vec<PathBuf>,
    pub(crate) links: BTreeMap<PathBuf, String>,
    pub(crate) left_out: Vec<LeftOut>,
}

impl Tree {
    /// The left-out path holding `path`, or equal to it.
    fn holding(&self, path: &Path) -> Option<&LeftOut> {
        self.left_out.iter().find(|l| path.starts_with(&l.path))
    }

    /// The left-out paths inside `dir`.
    pub(crate) fn under<'a>(&'a self, dir: &'a Path) -> impl Iterator<Item = &'a LeftOut> {
        self.left_out
            .iter()
            .filter(move |l| l.path.starts_with(dir))
    }

    fn leave_out(&mut self, path: PathBuf, reason: Reason) {
        if self.holding(&path).is_none() {
            self.left_out.push(LeftOut { path, reason });
        }
    }

    /// Keep the symlink at `path` as a link when it resolves inside the
    /// repository (`root`, canonical), else leave it out.
    fn link(&mut self, root: &Path, path: PathBuf) -> Result<()> {
        if self.links.contains_key(&path) || self.holding(&path).is_some() {
            return Ok(());
        }
        let target = fs::read_link(root.join(&path))?;
        match target.to_str() {
            _ if !cfg!(unix) => self.leave_out(path, Reason::NoSymlinks),
            None => self.leave_out(path, Reason::UnreadableLink),
            Some(text) if resolves_inside(root, &path, &target) => {
                self.links.insert(path, text.to_owned());
            }
            Some(_) => self.leave_out(path, Reason::OutsideLink),
        }
        Ok(())
    }

    /// Leave out every link that points into a left-out path, until none
    /// does (a link can point at another such link).
    fn drop_links_into_left_out(&mut self) {
        loop {
            let into: Vec<PathBuf> = self
                .links
                .iter()
                .filter(|(link, target)| {
                    let resolved = normalize(&link.parent().unwrap_or(Path::new("")).join(target));
                    self.holding(&resolved).is_some()
                })
                .map(|(link, _)| link.clone())
                .collect();
            if into.is_empty() {
                return;
            }
            for link in into {
                self.links.remove(&link);
                self.leave_out(link, Reason::IntoLeftOut);
            }
        }
    }
}

/// Whether the symlink at `link` (relative to `root`, canonical) points
/// inside the repository, both as written and as the file system resolves
/// it through any other links.
fn resolves_inside(root: &Path, link: &Path, target: &Path) -> bool {
    if target.is_absolute() {
        return false;
    }
    let mut depth = link.components().count().saturating_sub(1);
    for part in target.components() {
        match part {
            Component::Normal(_) => depth += 1,
            Component::CurDir => (),
            Component::ParentDir if depth > 0 => depth -= 1,
            _ => return false,
        }
    }
    match fs::canonicalize(root.join(link)) {
        Ok(real) => real.starts_with(root),
        // A dangling link stays what the repository has.
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    }
}

/// Walk the working tree (ADR 0007): in-repository symlinks are kept as
/// links; symlinks leaving the repository or pointing into a left-out path,
/// and nested repositories, are left out; tracked files are listed whatever
/// their directory is called.
pub(crate) fn walk(root: &Path, layout: &Layout) -> Result<Tree> {
    let root = &canonical(root)?;
    let skipped = layout.skip_dirs.clone();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .filter_entry(move |e| {
            e.depth() == 0
                || !e
                    .file_name()
                    .to_str()
                    .is_some_and(|n| n == ".git" || skipped.contains(n))
        })
        .build();
    let mut tree = Tree::default();
    let mut paths = vec![];
    for entry in walker {
        let entry = entry.map_err(|e| Error::Operation(e.to_string()))?;
        if entry.depth() == 0 {
            continue;
        }
        let path = entry
            .path()
            .strip_prefix(root)
            .map_err(|e| Error::Operation(e.to_string()))?
            .to_path_buf();
        if tree.holding(&path).is_some() {
            continue;
        }
        let kind = entry.file_type();
        if kind.is_some_and(|t| t.is_dir()) && entry.path().join(".git").exists() {
            tree.leave_out(path, Reason::Nested);
        } else if kind.is_some_and(|t| t.is_symlink()) {
            tree.link(root, path)?;
        } else if kind.is_some_and(|t| t.is_file()) {
            paths.push(path);
        }
    }
    // Git can track files that now match ignore rules. Omitting those files
    // changes SCM dirty-state detection (and can drop required build sources).
    if let Some(snapshot) = crate::scm::Snapshot::read(root)? {
        let tracked = snapshot.tracked(root)?;
        for module in tracked.submodules {
            tree.leave_out(module, Reason::Nested);
        }
        for link in tracked.links {
            tree.link(root, link)?;
        }
        paths.extend(
            tracked
                .files
                .into_iter()
                .filter(|p| tree.holding(p).is_none()),
        );
    }
    // Adapters' managed inputs (locks, native configuration) are resolver
    // inputs even when ignored. Probe only next to matching manifests; never
    // traverse installed environments.
    let manifests = paths.clone();
    for manifest in manifests {
        let name = manifest.file_name().and_then(|n| n.to_str()).unwrap_or("");
        for rule in &layout.managed {
            if !rule.manifests.iter().any(|p| matches_pattern(p, name)) {
                continue;
            }
            for input in &rule.inputs {
                let path = manifest.parent().unwrap().join(input);
                relative(&path)?;
                // The input, or a directory above it, may be a symlink the
                // walk never saw because it is ignored.
                let linked = path
                    .ancestors()
                    .filter(|a| !a.as_os_str().is_empty())
                    .find(|a| {
                        tree.links.contains_key(*a)
                            || tree.holding(a).is_some()
                            || fs::symlink_metadata(root.join(a))
                                .is_ok_and(|m| m.file_type().is_symlink())
                    });
                if let Some(link) = linked {
                    tree.link(root, link.to_path_buf())?;
                    continue;
                }
                output_path(root, &path)?;
                if root.join(&path).is_file() {
                    paths.push(path);
                }
            }
        }
    }
    tree.drop_links_into_left_out();
    tree.left_out.sort_by(|a, b| a.path.cmp(&b.path));
    paths.sort();
    paths.dedup();
    tree.files = paths;
    Ok(tree)
}

pub(crate) fn fingerprint(root: &Path, layout: &Layout) -> Result<BTreeMap<PathBuf, String>> {
    Ok(fingerprint_tree(root, layout)?.0)
}

/// The fingerprint of the inputs, and the tree they were read from.
pub(crate) fn fingerprint_tree(
    root: &Path,
    layout: &Layout,
) -> Result<(BTreeMap<PathBuf, String>, Tree)> {
    let tree = walk(root, layout)?;
    let mut inputs: BTreeMap<_, _> = tree
        .files
        .iter()
        .map(|path| {
            let hash = file_hash(&root.join(path))?;
            Ok((path.clone(), hash))
        })
        .collect::<Result<_>>()?;
    for (path, target) in &tree.links {
        inputs.insert(path.clone(), format!("{LINK}{target}"));
    }
    if let Some(snapshot) = crate::scm::Snapshot::read(root)? {
        for (relative, source) in snapshot.files {
            inputs.insert(relative, file_hash(&source)?);
        }
        inputs.insert(
            ".git/config".into(),
            format!("{:x}", Sha256::digest(snapshot.config.as_bytes())),
        );
    }
    Ok((inputs, tree))
}

/// The fingerprint prefix of an in-repository symlink, before its target.
const LINK: &str = "symlink:";

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
    layout: &Layout,
) -> Result<()> {
    for (path, hash) in inputs {
        if path.starts_with(".git") {
            continue;
        }
        let dest = destination.join(path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        match hash.strip_prefix(LINK) {
            #[cfg(unix)]
            Some(target) => std::os::unix::fs::symlink(target, dest)?,
            #[cfg(not(unix))]
            Some(_) => unreachable!("symlinks are only kept on unix"),
            None => {
                fs::copy(root.join(path), dest)?;
            }
        }
    }
    if let Some(snapshot) = crate::scm::Snapshot::read(root)? {
        snapshot.copy(destination)?;
    }
    if fingerprint(destination, layout)? != *inputs {
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

/// Without Windows' verbatim prefix: `\\?\C:\x` -> `C:\x` and
/// `\\?\UNC\srv\share` -> `\\srv\share`. Other text is returned unchanged.
fn without_verbatim_prefix(text: &str) -> std::borrow::Cow<'_, str> {
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}").into();
    }
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest.into(),
        _ => text.into(),
    }
}

/// `canonicalize` in a form native tools accept. Windows returns verbatim
/// paths (`\\?\C:\...`) that Git for Windows cannot open.
pub(crate) fn canonical(path: &Path) -> std::io::Result<PathBuf> {
    let path = path.canonicalize()?;
    if cfg!(windows) {
        if let Some(text) = path.to_str() {
            return Ok(PathBuf::from(without_verbatim_prefix(text).into_owned()));
        }
    }
    Ok(path)
}

/// Normalize `a/./b/../c` without touching the filesystem.
pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                output.pop();
            }
            other => output.push(other),
        }
    }
    output
}

/// `dir` and the directories below it, in order, skipping hidden ones and
/// `node_modules` as workspace globs do.
fn descendants(root: &Path, dir: &Path, output: &mut Vec<PathBuf>) {
    output.push(dir.to_path_buf());
    let mut names: Vec<String> = fs::read_dir(root.join(dir))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| !n.starts_with('.') && n != "node_modules")
        .collect();
    names.sort();
    for name in names {
        descendants(root, &dir.join(name), output);
    }
}

/// Directories under `root` matching `pattern` (with `*` components, and
/// `**` for any depth), relative to `base`.
pub(crate) fn expand(root: &Path, base: &Path, pattern: &str) -> Vec<PathBuf> {
    let mut current = vec![base.to_path_buf()];
    for component in pattern.split('/').filter(|c| !c.is_empty()) {
        let mut next = vec![];
        for dir in &current {
            if component == "**" {
                descendants(root, dir, &mut next);
            } else if component.contains('*') {
                let mut names: Vec<String> = fs::read_dir(root.join(dir))
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|n| matches_pattern(component, n))
                    .collect();
                names.sort();
                next.extend(names.into_iter().map(|n| dir.join(n)));
            } else {
                next.push(normalize(&dir.join(component)));
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        next.retain(|dir| seen.insert(dir.clone()));
        current = next;
    }
    current
}

#[cfg(test)]
mod tests {
    use super::Layout;

    fn layout() -> Layout {
        Layout::new(&crate::Engine::default().specs())
    }

    #[test]
    fn verbatim_prefixes_are_removed_for_native_tools() {
        use super::without_verbatim_prefix as plain;
        assert_eq!(
            plain(r"\\?\C:\Users\a\.git\config"),
            r"C:\Users\a\.git\config"
        );
        assert_eq!(plain(r"\\?\UNC\server\share\repo"), r"\\server\share\repo");
        assert_eq!(plain(r"\\?\Volume{0}\x"), r"\\?\Volume{0}\x");
        assert_eq!(plain("/home/user/repo"), "/home/user/repo");
        assert_eq!(plain(r"C:\already\plain"), r"C:\already\plain");
    }
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

    fn left_out(root: &Path) -> Vec<(PathBuf, String)> {
        walk(root, &layout())
            .unwrap()
            .left_out
            .iter()
            .map(|l| (l.path.clone(), l.reason.to_string()))
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_directories_above_managed_inputs_are_staged_or_left_out() {
        let root = git_project();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("config.toml"), "[mirrors]\n").unwrap();
        fs::write(root.path().join(".gitignore"), ".pixi\n").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join(".pixi")).unwrap();
        fs::create_dir_all(root.path().join("rs/shared")).unwrap();
        fs::write(root.path().join("rs/Cargo.toml"), "[package]\nname='a'\n").unwrap();
        fs::write(root.path().join("rs/shared/config.toml"), "[net]\n").unwrap();
        std::os::unix::fs::symlink("shared", root.path().join("rs/.cargo")).unwrap();
        let inputs = fingerprint(root.path(), &layout()).unwrap();
        assert_eq!(inputs[Path::new("rs/.cargo")], "symlink:shared");
        assert!(inputs.contains_key(Path::new("rs/shared/config.toml")));
        assert_eq!(
            left_out(root.path()),
            [(
                PathBuf::from(".pixi"),
                "symlink outside the repository".into()
            )]
        );
    }

    #[cfg(unix)]
    #[test]
    fn tracked_symlinks_under_skipped_directories_are_staged() {
        let root = git_project();
        fs::create_dir(root.path().join("node_modules")).unwrap();
        std::os::unix::fs::symlink("../tracked.txt", root.path().join("node_modules/l")).unwrap();
        git(root.path(), &["add", "-f", "node_modules/l"]);
        git(root.path(), &["commit", "-m", "tracked link"]);
        let inputs = fingerprint(root.path(), &layout()).unwrap();
        assert_eq!(
            inputs[Path::new("node_modules/l")],
            "symlink:../tracked.txt"
        );
    }

    #[cfg(unix)]
    #[test]
    fn links_that_resolve_outside_or_into_left_out_paths_are_left_out() {
        let root = git_project();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("f"), "x\n").unwrap();
        for dir in ["a", "b"] {
            fs::create_dir(root.path().join(dir)).unwrap();
        }
        std::os::unix::fs::symlink(outside.path(), root.path().join("b/data")).unwrap();
        std::os::unix::fs::symlink("../b/data/..", root.path().join("b/sneaky")).unwrap();
        std::os::unix::fs::symlink("../b/data/f", root.path().join("a/x")).unwrap();
        fs::create_dir(root.path().join("nested")).unwrap();
        git(&root.path().join("nested"), &["init"]);
        fs::write(root.path().join("nested/f"), "x\n").unwrap();
        std::os::unix::fs::symlink("../nested/f", root.path().join("a/y")).unwrap();
        let outside_link = "symlink outside the repository".to_string();
        assert_eq!(
            left_out(root.path()),
            [
                (PathBuf::from("a/x"), outside_link.clone()),
                (PathBuf::from("a/y"), "symlink into a left-out path".into()),
                (PathBuf::from("b/data"), outside_link.clone()),
                (PathBuf::from("b/sneaky"), outside_link),
                (
                    PathBuf::from("nested"),
                    "nested Git repository or submodule".into()
                ),
            ]
        );
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
        let inputs = fingerprint(root.path(), &layout()).unwrap();
        stage(root.path(), destination.path(), &inputs, &layout()).unwrap();
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

    #[cfg(unix)]
    #[test]
    fn in_repository_symlinks_are_recreated_in_the_stage_and_fingerprinted_by_target() {
        let root = git_project();
        fs::write(root.path().join("AGENTS.md"), "notes\n").unwrap();
        std::os::unix::fs::symlink("AGENTS.md", root.path().join("CLAUDE.md")).unwrap();
        fs::create_dir(root.path().join("docs")).unwrap();
        std::os::unix::fs::symlink("../tracked.txt", root.path().join("docs/linked.txt")).unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-m", "links"]);
        let destination = tempfile::tempdir().unwrap();
        let inputs = fingerprint(root.path(), &layout()).unwrap();
        stage(root.path(), destination.path(), &inputs, &layout()).unwrap();
        for (link, target) in [
            ("CLAUDE.md", "AGENTS.md"),
            ("docs/linked.txt", "../tracked.txt"),
        ] {
            assert_eq!(
                fs::read_link(destination.path().join(link)).unwrap(),
                Path::new(target)
            );
        }
        assert_eq!(
            git(destination.path(), &["status", "--porcelain"]),
            "",
            "the stage matches the commit"
        );
        fs::remove_file(root.path().join("CLAUDE.md")).unwrap();
        std::os::unix::fs::symlink("tracked.txt", root.path().join("CLAUDE.md")).unwrap();
        assert_ne!(inputs, fingerprint(root.path(), &layout()).unwrap());
    }

    #[test]
    fn git_tags_and_index_changes_invalidate_fingerprint() {
        let root = git_project();
        let before = fingerprint(root.path(), &layout()).unwrap();
        git(root.path(), &["tag", "v2.0.0"]);
        assert_ne!(before, fingerprint(root.path(), &layout()).unwrap());
        let before = fingerprint(root.path(), &layout()).unwrap();
        fs::write(root.path().join("tracked.txt"), "staged\n").unwrap();
        git(root.path(), &["add", "tracked.txt"]);
        fs::write(root.path().join("tracked.txt"), "original\n").unwrap();
        assert_ne!(before, fingerprint(root.path(), &layout()).unwrap());
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
            &fingerprint(&worktree, &layout()).unwrap(),
            &layout(),
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
        assert!(matches!(
            fingerprint(root.path(), &layout()),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn ancestor_git_repositories_are_rejected_and_nested_ones_left_out() {
        let root = git_project();
        let child = root.path().join("nested");
        fs::create_dir(&child).unwrap();
        fs::write(child.join("pixi.toml"), "[workspace]\nname='nested'\n").unwrap();
        assert!(matches!(
            fingerprint(&child, &layout()),
            Err(Error::Invalid(_))
        ));
        git(&child, &["init"]);
        let inputs = fingerprint(root.path(), &layout()).unwrap();
        assert!(inputs.contains_key(Path::new("pixi.toml")));
        assert!(!inputs.keys().any(|p| p.starts_with("nested")));
        assert_eq!(
            left_out(root.path()),
            [(
                PathBuf::from("nested"),
                "nested Git repository or submodule".into()
            )]
        );
    }

    #[test]
    fn tracked_files_under_skipped_directory_names_and_submodules_are_staged_or_left_out() {
        let root = git_project();
        fs::create_dir_all(root.path().join("src/target")).unwrap();
        fs::write(root.path().join("src/target/legacy.rs"), "// source\n").unwrap();
        let module = git_project();
        git(
            root.path(),
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                module.path().to_str().unwrap(),
                "vendor/module",
            ],
        );
        git(root.path(), &["add", "."]);
        git(
            root.path(),
            &["commit", "-m", "source under target, and a submodule"],
        );
        let destination = tempfile::tempdir().unwrap();
        let inputs = fingerprint(root.path(), &layout()).unwrap();
        stage(root.path(), destination.path(), &inputs, &layout()).unwrap();
        assert!(destination.path().join("src/target/legacy.rs").is_file());
        assert!(!inputs.keys().any(|p| p.starts_with("vendor/module")));
        assert_eq!(
            left_out(root.path()),
            [(
                PathBuf::from("vendor/module"),
                "nested Git repository or submodule".into()
            )]
        );
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
        assert!(matches!(
            fingerprint(root.path(), &layout()),
            Err(Error::Invalid(_))
        ));
        fs::remove_file(root.path().join(".git/objects/info/alternates")).unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("exclude"), "secret\n").unwrap();
        fs::remove_dir_all(root.path().join(".git/info")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join(".git/info")).unwrap();
        assert!(matches!(
            fingerprint(root.path(), &layout()),
            Err(Error::Invalid(_))
        ));
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
        let inputs = fingerprint(source.path(), &layout()).unwrap();
        assert!(inputs.contains_key(Path::new("pixi.lock")));
        stage(source.path(), destination.path(), &inputs, &layout()).unwrap();
        assert_eq!(
            fs::read_to_string(destination.path().join("pixi.lock")).unwrap(),
            "existing baseline"
        );
        fs::write(source.path().join("pixi.lock"), "changed baseline").unwrap();
        assert_ne!(inputs, fingerprint(source.path(), &layout()).unwrap());
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
        let inputs = fingerprint(source.path(), &layout()).unwrap();
        assert!(inputs.contains_key(Path::new(".pixi/config.toml")));
        stage(source.path(), stage_dir.path(), &inputs, &layout()).unwrap();
        assert!(!stage_dir.path().join(".pixi/envs").exists());
        fs::write(source.path().join(".pixi/config.toml"), "changed").unwrap();
        assert_ne!(inputs, fingerprint(source.path(), &layout()).unwrap());
    }
}

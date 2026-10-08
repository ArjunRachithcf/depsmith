//! The uv adapter: each `uv.lock` owner (a workspace root or a standalone
//! project) is a target whose declarations are the PEP 508 requirement lists
//! of its `pyproject.toml` files. uv resolves in the stage; Git pins are kept
//! unless `--refresh-git` is selected.
use crate::{
    adapter::{
        edits_by_manifest, pypi_key, undeclared, Adapter, AdapterSpec, Candidate, Capabilities,
        LockCheck, ManagedFiles, Support, ToolSpec,
    },
    constraints::{AvailabilityConfig, Declaration, Edit, RegistryConfig},
    graph::{LockGraph, Node, Requirement},
    process::run_env,
    Error, Package, Result, Target, UpdateOptions,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

/// The uv adapter. Availability evidence comes from the project's indexes
/// unless `registry` replaces them (for tests and mirrors).
#[derive(Default)]
pub struct Uv {
    /// Registry consulted for newer releases instead of the project's indexes.
    pub registry: Option<RegistryConfig>,
}

fn parse(text: &str, path: &Path) -> Result<toml::Value> {
    text.parse()
        .map_err(|e| Error::Invalid(format!("invalid manifest {}: {e}", path.display())))
}

fn read(root: &Path, relative: &Path) -> Result<toml::Value> {
    parse(&fs::read_to_string(root.join(relative))?, relative)
}

fn tool<'a>(parsed: &'a toml::Value, name: &str) -> Option<&'a toml::Value> {
    parsed.get("tool")?.get(name)
}

fn strings(value: Option<&toml::Value>) -> Vec<&str> {
    value
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_str)
        .collect()
}

/// Member manifests of the workspace whose root manifest is in `dir`, in
/// order, excluding `exclude` patterns and the root itself.
fn members(root: &Path, dir: &Path, workspace: &toml::Value) -> Vec<PathBuf> {
    let excluded: Vec<PathBuf> = strings(workspace.get("exclude"))
        .into_iter()
        .flat_map(|e| crate::working_tree::expand(root, dir, e))
        .collect();
    let mut output = vec![];
    for pattern in strings(workspace.get("members")) {
        for member in crate::working_tree::expand(root, dir, pattern) {
            let manifest = member.join("pyproject.toml");
            if member != dir
                && !excluded.contains(&member)
                && root.join(&manifest).is_file()
                && !output.contains(&manifest)
            {
                output.push(manifest);
            }
        }
    }
    output
}

/// The uv settings of the project at `manifest`: its `uv.toml` when there is
/// one (uv then ignores `[tool.uv]`), else the pyproject's `[tool.uv]`.
fn settings(root: &Path, manifest: &Path) -> Result<Option<toml::Value>> {
    let uv_toml = manifest.with_file_name("uv.toml");
    if root.join(&uv_toml).is_file() {
        return read(root, &uv_toml).map(Some);
    }
    Ok(tool(&read(root, manifest)?, "uv").cloned())
}

/// Lists uv reads besides the standard `[project]` and `[dependency-groups]`.
const UV_LISTS: [&str; 1] = ["tool.uv.dev-dependencies"];

/// The manifests of `target`: its own, then its workspace members.
fn manifests(root: &Path, target: &Target) -> Result<Vec<PathBuf>> {
    let parsed = read(root, &target.manifest)?;
    let mut output = vec![target.manifest.clone()];
    if let Some(workspace) = tool(&parsed, "uv").and_then(|u| u.get("workspace")) {
        let dir = target.manifest.parent().unwrap_or(Path::new(""));
        output.extend(members(root, dir, workspace));
    }
    Ok(output)
}

/// Normalized names with a `[tool.uv.sources]` entry in `parsed`. Git,
/// path, URL and workspace sources are not index releases, and a package
/// pinned to a named index resolves only there, so none of them is looked up
/// on the project's indexes.
fn sourced(parsed: &toml::Value) -> Vec<String> {
    tool(parsed, "uv")
        .and_then(|u| u.get("sources"))
        .and_then(toml::Value::as_table)
        .into_iter()
        .flatten()
        .map(|(name, _)| pypi_key(name))
        .collect()
}

/// A requirement of the target and whether the project's indexes resolve it.
struct Entry {
    declaration: Declaration,
    from_indexes: bool,
}

/// Every requirement of the target. The workspace root's sources apply to
/// every member, alongside the member's own.
fn entries(root: &Path, target: &Target) -> Result<Vec<Entry>> {
    let workspace_sources = sourced(&read(root, &target.manifest)?);
    let mut output = vec![];
    for manifest in manifests(root, target)? {
        let text = fs::read_to_string(root.join(&manifest))?;
        let member_sources = sourced(&parse(&text, &manifest)?);
        for declaration in crate::pyproject::declarations(&text, &manifest, &UV_LISTS)? {
            let key = pypi_key(&declaration.package);
            let from_indexes = !workspace_sources.contains(&key) && !member_sources.contains(&key);
            output.push(Entry {
                declaration,
                from_indexes,
            });
        }
    }
    Ok(output)
}

fn parse_lock(text: &str) -> Result<toml::Value> {
    let parsed: toml::Value = text
        .parse()
        .map_err(|e| Error::Invalid(format!("invalid uv.lock: {e}")))?;
    let version = parsed.get("version").and_then(toml::Value::as_integer);
    if version != Some(1) {
        return Err(Error::Invalid(format!(
            "unsupported uv.lock version {}; supported: 1",
            version.map_or_else(|| "(missing)".into(), |v| v.to_string())
        )));
    }
    Ok(parsed)
}

/// The `uv.lock` beside a target's manifest.
fn lock_path(target: &Target) -> PathBuf {
    target.manifest.with_file_name("uv.lock")
}

/// The `[[package]]` rows of a parsed `uv.lock`.
fn packages(parsed: &toml::Value) -> impl Iterator<Item = &toml::Value> {
    parsed
        .get("package")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
}

/// A row's dependency lists: `dependencies`, then each dependency group.
fn dependency_lists(row: &toml::Value) -> impl Iterator<Item = &toml::Value> {
    let groups = row
        .get("dev-dependencies")
        .and_then(toml::Value::as_table)
        .into_iter()
        .flat_map(|groups| groups.values());
    row.get("dependencies").into_iter().chain(groups)
}

/// The `name` of a lock row or dependency entry.
fn name_of(value: &toml::Value) -> Option<&str> {
    value.get("name").and_then(toml::Value::as_str)
}

/// The lock graph of a `uv.lock`: one `pypi` node per package, on platform
/// `any` (uv locks every platform together). uv records dependency names
/// only, so requirements have no `spec`.
///
/// A package's edges are its dependencies, its dependency groups, and the
/// requirements of each of its extras that some locked package asks for
/// (`{ name = "requests", extra = ["socks"] }`). Extras nobody asks for, such
/// as the project's own, are not installed by default and are left out.
///
/// # Errors
///
/// Returns [`Error::Invalid`] when the lock is not valid TOML or has an
/// unsupported `version`.
pub fn lock_graph(text: &str) -> Result<LockGraph> {
    let parsed = parse_lock(text)?;
    let mut requested: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for row in packages(&parsed) {
        for dependency in
            dependency_lists(row).flat_map(|list| list.as_array().into_iter().flatten())
        {
            let (Some(name), Some(extras)) = (
                name_of(dependency),
                dependency.get("extra").and_then(toml::Value::as_array),
            ) else {
                continue;
            };
            requested
                .entry(name)
                .or_default()
                .extend(extras.iter().filter_map(toml::Value::as_str));
        }
    }

    let mut nodes = vec![];
    for row in packages(&parsed) {
        let Some(name) = name_of(row) else {
            continue;
        };
        let optional = row
            .get("optional-dependencies")
            .and_then(toml::Value::as_table);
        let extras = requested
            .get(name)
            .into_iter()
            .flatten()
            .filter_map(|extra| optional?.get(*extra));
        let mut requires: Vec<Requirement> = vec![];
        for dependencies in dependency_lists(row).chain(extras) {
            for required in dependencies
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(name_of)
            {
                if !requires.iter().any(|r| r.name == required) {
                    requires.push(Requirement {
                        name: required.into(),
                        spec: None,
                    });
                }
            }
        }
        nodes.push(Node {
            ecosystem: "pypi".into(),
            name: name.into(),
            platform: "any".into(),
            requires,
        });
    }
    Ok(LockGraph { nodes })
}

/// Resolved packages of a `uv.lock`. Virtual, editable and path packages are
/// the project itself (or local code) and are omitted. An index package's
/// artifact is its sdist, else its first wheel (under the registry directory
/// for a file registry); a Git package's is its `git+` URL with the locked
/// commit.
///
/// # Errors
///
/// Returns [`Error::Invalid`] when the lock is not valid TOML or has an
/// unsupported `version`.
pub fn lock_inventory(text: &str) -> Result<Vec<Package>> {
    let parsed = parse_lock(text)?;
    let mut inventory = vec![];
    for row in packages(&parsed) {
        let field = |name: &str| row.get(name).and_then(toml::Value::as_str);
        let (Some(name), Some(version), Some(source)) =
            (field("name"), field("version"), row.get("source"))
        else {
            continue;
        };
        let registry = source.get("registry").and_then(toml::Value::as_str);
        // A file registry (a directory of distributions) gives relative paths.
        let url = |value: Option<&toml::Value>| {
            let value = value?;
            if let Some(url) = value.get("url").and_then(toml::Value::as_str) {
                return Some(url.to_owned());
            }
            let path = value.get("path").and_then(toml::Value::as_str)?;
            Some(format!("{}/{path}", registry?.trim_end_matches('/')))
        };
        let artifact = if registry.is_some() {
            url(row.get("sdist")).or_else(|| {
                url(row
                    .get("wheels")
                    .and_then(toml::Value::as_array)
                    .and_then(|w| w.first()))
            })
        } else if let Some(git) = source.get("git").and_then(toml::Value::as_str) {
            Some(format!("git+{git}"))
        } else {
            source
                .get("url")
                .and_then(toml::Value::as_str)
                .map(str::to_owned)
        };
        let Some(artifact) = artifact else {
            continue;
        };
        inventory.push(Package {
            ecosystem: "pypi".into(),
            name: name.into(),
            version: version.into(),
            artifact,
            platform: "any".into(),
        });
    }
    Ok(inventory)
}

impl Adapter for Uv {
    fn spec(&self) -> AdapterSpec {
        AdapterSpec {
            manager: "uv".into(),
            ecosystems: vec!["pypi".into()],
            patterns: vec!["pyproject.toml".into()],
            managed: vec![ManagedFiles {
                manifests: vec!["pyproject.toml".into()],
                inputs: vec!["uv.lock".into(), "uv.toml".into(), ".python-version".into()],
            }],
            skip_dirs: vec![".venv".into()],
            tools: vec![ToolSpec {
                name: "uv".into(),
                default: "uv".into(),
                tested_versions: vec!["0.12.15".into()],

                downloads: crate::provision::pinned("uv"),
            }],
            capabilities: Capabilities {
                package_selection: Support::Supported,
                constraint_changes: Support::Unsupported(
                    "change a requirement with --accept NAME or --accept NAME=REQUIREMENT".into(),
                ),
                suggestion_acceptance: Support::Supported,
                git_refresh: Support::Supported,
                cooldown: Support::Unsupported(
                    "a common cooldown is not implemented; configure tool.uv.exclude-newer in pyproject.toml".into(),
                ),
                install_validation: Support::Unsupported(
                    "installing the candidate is not implemented for uv".into(),
                ),
                lockfile: Support::Supported,
                // uv.lock is universal: it covers every platform.
                platforms: Support::Supported,
            },
        }
    }
    /// A pyproject uv manages: one with a `[tool.uv]` table and no
    /// `[tool.pixi]` table (that one is Pixi's).
    fn detects(&self, _: &Path, content: &str) -> bool {
        content
            .parse::<toml::Value>()
            .is_ok_and(|v| tool(&v, "uv").is_some() && tool(&v, "pixi").is_none())
    }
    /// A pyproject with a `uv.lock` beside it or a `[tool.uv]` table, not
    /// managed by Pixi, and not a member of an enclosing uv workspace:
    /// exactly the projects that own a `uv.lock`.
    fn detects_in(&self, root: &Path, relative: &Path, content: &str) -> bool {
        let Ok(parsed) = content.parse::<toml::Value>() else {
            return false;
        };
        if tool(&parsed, "pixi").is_some() {
            return false;
        }
        let dir = relative.parent().unwrap_or(Path::new(""));
        if tool(&parsed, "uv").is_none() && !root.join(dir).join("uv.lock").is_file() {
            return false;
        }
        for ancestor in dir.ancestors().skip(1) {
            let Ok(manifest) = read(root, &ancestor.join("pyproject.toml")) else {
                continue;
            };
            if let Some(workspace) = tool(&manifest, "uv").and_then(|u| u.get("workspace")) {
                return !members(root, ancestor, workspace).contains(&relative.to_path_buf());
            }
        }
        true
    }
    fn inventory(&self, root: &Path, target: &Target) -> Result<Vec<Package>> {
        let lock = lock_path(target);
        match fs::read_to_string(root.join(&lock)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::Invalid(format!(
                "{}: no {} to scan; create it with `uv lock` or `depsmith update`",
                target.id,
                lock.display()
            ))),
            result => lock_inventory(&result?),
        }
    }
    fn lock_graph(&self, root: &Path, target: &Target) -> Result<Option<LockGraph>> {
        match fs::read_to_string(root.join(lock_path(target))) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            result => lock_graph(&result?).map(Some),
        }
    }
    fn select(
        &self,
        root: &Path,
        target: &Target,
        requested: &[String],
    ) -> Result<BTreeMap<String, String>> {
        let declared = entries(root, target)?;
        let mut selected = BTreeMap::new();
        for request in requested {
            let key = pypi_key(request);
            if let Some(entry) = declared
                .iter()
                .find(|e| pypi_key(&e.declaration.package) == key)
            {
                selected.insert(request.clone(), entry.declaration.package.clone());
            }
        }
        Ok(selected)
    }
    /// Requirements the project's indexes resolve; packages with a
    /// `[tool.uv.sources]` entry are not consulted.
    fn declarations(&self, root: &Path, target: &Target) -> Result<Vec<Declaration>> {
        Ok(entries(root, target)?
            .into_iter()
            .filter(|e| e.from_indexes)
            .map(|e| e.declaration)
            .collect())
    }
    fn rewrite(&self, stage: &Path, target: &Target, edits: &[Edit]) -> Result<()> {
        let manifests = manifests(stage, target)?;
        for (file, edits) in edits_by_manifest(target, &manifests, edits)? {
            let path = stage.join(file);
            let text = rewrite_manifest(&fs::read_to_string(&path)?, &edits)?;
            fs::write(path, text)?;
        }
        Ok(())
    }
    /// PyPI packages are looked up on the project's indexes under its
    /// `exclude-newer`.
    fn availability(&self, root: &Path, target: &Target) -> Result<AvailabilityConfig> {
        let settings = settings(root, &target.manifest)?;
        let registry = self
            .registry
            .clone()
            .unwrap_or_else(|| RegistryConfig::PypiSimple {
                indexes: crate::pypi::uv_index_urls(settings.as_ref()),
            });
        Ok(AvailabilityConfig {
            registries: BTreeMap::from([("pypi".into(), registry)]),
            exclude_newer: settings
                .as_ref()
                .and_then(|s| s.get("exclude-newer"))
                .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
                .map(|text| {
                    // uv reads a bare date in the local time zone, which the
                    // engine cannot know: leave availability unestablished.
                    if crate::cutoff::is_date(&text) {
                        format!("{text} (a local date in uv)")
                    } else {
                        text
                    }
                }),
        })
    }
    fn prepare(&self, stage: &Path, target: &Target, options: &UpdateOptions) -> Result<Candidate> {
        if options.upgrade {
            return Err(Error::Invalid(
                "--upgrade is not supported by the uv adapter".into(),
            ));
        }
        let uv = options.tool("uv");
        let timeout = options.timeout_seconds;
        let dir = stage.join(target.manifest.parent().unwrap_or(Path::new("")));
        let check = LockCheck::take(
            stage,
            manifests(stage, target)?,
            lock_path(target),
            lock_inventory,
        )?;
        let lock = |extra: &[&str]| -> Vec<String> {
            let mut args: Vec<String> = ["lock", "--color", "never", "--project"]
                .map(str::to_owned)
                .into();
            args.push(dir.to_string_lossy().into());
            args.extend(extra.iter().map(|a| a.to_string()));
            args
        };
        let upgrades: Vec<&str> = if !options.packages.is_empty() {
            options.packages.iter().map(String::as_str).collect()
        } else if !options.refresh_git && check.has_git_pins() {
            // `--upgrade` would move Git pins: upgrade every other package.
            let names: BTreeSet<&str> = check
                .before()
                .iter()
                .filter(|p| !p.artifact.starts_with("git+"))
                .map(|p| p.name.as_str())
                .collect();
            names.into_iter().collect()
        } else {
            vec![]
        };
        let mut args = lock(&[]);
        if upgrades.is_empty() {
            args.push("--upgrade".into());
        }
        for name in upgrades {
            args.extend(["--upgrade-package".into(), name.to_owned()]);
        }
        run_env(&uv, &args, &dir, timeout, options.tool_env("uv"))?;
        let resolved = check.resolved(stage, options.refresh_git)?;
        run_env(
            &uv,
            &lock(&["--locked"]),
            &dir,
            timeout,
            options.tool_env("uv"),
        )?;
        check.candidate(stage, target, &resolved, vec![])
    }
}

/// Apply the edits of one manifest's text, keeping comments and layout.
fn rewrite_manifest(text: &str, edits: &[&Edit]) -> Result<String> {
    let mut document: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| Error::Invalid(format!("invalid manifest: {e}")))?;
    let mut pending: Vec<&Edit> = edits.to_vec();
    {
        let mut change = |location: &str, package: &str, old: &str| {
            let index = pending.iter().position(|e| {
                let d = &e.declaration;
                d.location == location && d.package == package && d.requirement == old
            })?;
            Some(pending.swap_remove(index).requirement.clone())
        };
        crate::pyproject::rewrite(&mut document, &mut change, &UV_LISTS);
    }
    if let Some(edit) = pending.first() {
        return Err(undeclared(&edit.declaration));
    }
    Ok(document.to_string())
}

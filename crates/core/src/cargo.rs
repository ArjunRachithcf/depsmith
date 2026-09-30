//! The Cargo adapter: each `Cargo.lock` owner (a workspace root or a
//! standalone crate) is a target whose declarations are the dependency tables
//! of its manifests. Cargo resolves in the stage with MSRV-aware fallback;
//! crates held back by `rust-version` are reported as suggestions.
use crate::{
    adapter::{Adapter, AdapterSpec, Candidate, Capabilities, ManagedFiles, Support, ToolSpec},
    constraint::Excluded,
    constraints::{AvailabilityConfig, Declaration, Edit, RegistryConfig},
    process::{run, run_env_output},
    Error, Package, Result, Suggestion, Target, UpdateOptions,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

/// Where crates.io serves crate files; `{CRATES_IO_DOWNLOAD}/{name}/{version}/download`.
pub(crate) const CRATES_IO_DOWNLOAD: &str = "https://crates.io/api/v1/crates";
const CRATES_IO_INDEX: &str = "https://index.crates.io/";
const DEPENDENCY_TABLES: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];

/// The Cargo adapter. Availability evidence comes from the crates.io sparse
/// index unless `registry` replaces it (for tests and mirrors).
#[derive(Default)]
pub struct Cargo {
    /// Registry consulted for newer crate releases instead of crates.io.
    pub registry: Option<RegistryConfig>,
}

fn parse(text: &str, path: &Path) -> Result<toml::Value> {
    text.parse()
        .map_err(|e| Error::Invalid(format!("invalid manifest {}: {e}", path.display())))
}

fn read(root: &Path, relative: &Path) -> Result<toml::Value> {
    parse(&fs::read_to_string(root.join(relative))?, relative)
}

fn strings(value: Option<&toml::Value>) -> Vec<&str> {
    value
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_str)
        .collect()
}

/// Normalize `a/./b/../c` without touching the filesystem.
fn normalize(path: &Path) -> PathBuf {
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

/// Directories under `root` matching `pattern` (with `*` components),
/// relative to `base`.
fn expand(root: &Path, base: &Path, pattern: &str) -> Vec<PathBuf> {
    let mut current = vec![base.to_path_buf()];
    for component in pattern.split('/').filter(|c| !c.is_empty()) {
        let mut next = vec![];
        for dir in &current {
            if component.contains('*') {
                let mut names: Vec<String> = fs::read_dir(root.join(dir))
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|n| crate::working_tree::matches_pattern(component, n))
                    .collect();
                names.sort();
                next.extend(names.into_iter().map(|n| dir.join(n)));
            } else {
                next.push(normalize(&dir.join(component)));
            }
        }
        current = next;
    }
    current
}

/// Member manifests of the workspace whose root manifest is in `dir`, in
/// order, excluding `workspace.exclude` and the root itself.
fn members(root: &Path, dir: &Path, workspace: &toml::Value) -> Vec<PathBuf> {
    let excluded: Vec<PathBuf> = strings(workspace.get("exclude"))
        .into_iter()
        .map(|e| normalize(&dir.join(e)))
        .collect();
    let mut output = vec![];
    for pattern in strings(workspace.get("members")) {
        for member in expand(root, dir, pattern) {
            let manifest = member.join("Cargo.toml");
            if member != dir
                && !excluded.iter().any(|e| member.starts_with(e))
                && root.join(&manifest).is_file()
                && !output.contains(&manifest)
            {
                output.push(manifest);
            }
        }
    }
    output
}

/// The manifests of `target`: its own, then its workspace members.
fn manifests(root: &Path, target: &Target) -> Result<Vec<PathBuf>> {
    let parsed = read(root, &target.manifest)?;
    let mut output = vec![target.manifest.clone()];
    if let Some(workspace) = parsed.get("workspace") {
        let dir = target.manifest.parent().unwrap_or(Path::new(""));
        output.extend(members(root, dir, workspace));
    }
    Ok(output)
}

/// A dependency entry as (package, requirement, registry is crates.io).
/// Path, Git and workspace-inherited dependencies have no requirement.
fn entry(key: &str, spec: &toml::Value) -> (String, String, bool) {
    if let Some(version) = spec.as_str() {
        return (key.into(), version.into(), true);
    }
    let field = |name: &str| spec.get(name).and_then(toml::Value::as_str);
    let package = field("package").unwrap_or(key).to_owned();
    let crates_io = field("registry").is_none_or(|r| r == "crates-io");
    let local = ["path", "git", "workspace"]
        .iter()
        .any(|k| spec.get(k).is_some());
    let requirement = match field("version") {
        Some(version) if !local => version.to_owned(),
        _ => String::new(),
    };
    (package, requirement, crates_io)
}

/// Dependency tables of a manifest by location prefix: top-level tables,
/// platform-specific ones and `[workspace.dependencies]`.
fn tables(parsed: &toml::Value) -> Vec<(String, &toml::Table)> {
    fn add<'a>(
        output: &mut Vec<(String, &'a toml::Table)>,
        prefix: String,
        value: &'a toml::Value,
    ) {
        for (key, table) in value.as_table().into_iter().flatten() {
            if DEPENDENCY_TABLES.contains(&key.as_str()) {
                if let Some(table) = table.as_table() {
                    output.push((format!("{prefix}{key}"), table));
                }
            }
        }
    }
    let mut output = vec![];
    add(&mut output, String::new(), parsed);
    for (platform, value) in parsed
        .get("target")
        .and_then(toml::Value::as_table)
        .into_iter()
        .flatten()
    {
        add(&mut output, format!("target.{platform}."), value);
    }
    if let Some(workspace) = parsed.get("workspace") {
        add(&mut output, "workspace.".into(), workspace);
    }
    output.sort_by(|a, b| a.0.cmp(&b.0));
    output
}

/// Crate names compare case-insensitively with `-` and `_` equivalent.
fn crate_key(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace('_', "-")
}

/// Every dependency of the target as (declaration, registry is crates.io).
fn entries(root: &Path, target: &Target) -> Result<Vec<(Declaration, bool)>> {
    let mut output = vec![];
    for manifest in manifests(root, target)? {
        let parsed = read(root, &manifest)?;
        for (prefix, table) in tables(&parsed) {
            for (key, spec) in table {
                let (package, requirement, crates_io) = entry(key, spec);
                output.push((
                    Declaration {
                        ecosystem: "cargo".into(),
                        package,
                        requirement,
                        file: manifest.clone(),
                        location: format!("{prefix}.{key}"),
                    },
                    crates_io,
                ));
            }
        }
    }
    Ok(output)
}

/// Apply the edits of one manifest's text, keeping comments and layout.
fn rewrite_manifest(text: &str, edits: &[&Edit]) -> Result<String> {
    let mut document: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| Error::Invalid(format!("invalid manifest: {e}")))?;
    for edit in edits {
        let d = &edit.declaration;
        let (table, key) = d.location.rsplit_once('.').unwrap_or(("", &d.location));
        let mut item = document.as_item_mut();
        // Platform keys such as `cfg(unix)` never contain dots.
        for part in table.split('.').filter(|p| !p.is_empty()) {
            item = item.get_mut(part).ok_or_else(|| missing(d))?;
        }
        let slot = item.get_mut(key).ok_or_else(|| missing(d))?;
        let value = if slot.is_str() {
            slot.as_value_mut()
        } else {
            slot.as_table_like_mut()
                .and_then(|t| t.get_mut("version"))
                .and_then(|v| v.as_value_mut())
        };
        match value {
            Some(value) if value.as_str() == Some(d.requirement.as_str()) => {
                crate::pixi::replace_string(value, &edit.requirement);
            }
            _ => return Err(missing(d)),
        }
    }
    Ok(document.to_string())
}

fn missing(d: &Declaration) -> Error {
    Error::Invalid(format!(
        "{} {} is not declared at {} in {}",
        d.package,
        d.requirement,
        d.location,
        d.file.display()
    ))
}

/// Resolved packages of a `Cargo.lock`. Workspace members and other local
/// crates have no source and are the project itself, so they are omitted.
///
/// # Errors
///
/// Returns [`Error::Invalid`] when the lock is not valid TOML.
pub fn lock_inventory(text: &str) -> Result<Vec<Package>> {
    let parsed: toml::Value = text
        .parse()
        .map_err(|e| Error::Invalid(format!("invalid Cargo.lock: {e}")))?;
    let mut packages = vec![];
    for row in parsed
        .get("package")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
    {
        let field = |name: &str| row.get(name).and_then(toml::Value::as_str);
        let (Some(name), Some(version), Some(source)) =
            (field("name"), field("version"), field("source"))
        else {
            continue;
        };
        let crates_io = source == "registry+https://github.com/rust-lang/crates.io-index"
            || source == format!("sparse+{CRATES_IO_INDEX}");
        packages.push(Package {
            ecosystem: "cargo".into(),
            name: name.into(),
            version: version.into(),
            artifact: if crates_io {
                format!("{CRATES_IO_DOWNLOAD}/{name}/{version}/download")
            } else {
                source.into()
            },
            platform: "any".into(),
        });
    }
    Ok(packages)
}

/// Path of a crate's file in a sparse index.
fn sparse_path(name: &str) -> String {
    let name = name.to_ascii_lowercase();
    match name.len() {
        1 => format!("1/{name}"),
        2 => format!("2/{name}"),
        3 => format!("3/{}/{name}", &name[..1]),
        _ => format!("{}/{}/{name}", &name[..2], &name[2..4]),
    }
}

/// Newest release excluded by `requirement` among `(version, url, checksum)`
/// rows: stable versions only; empty when the requirement admits the newest.
pub(crate) fn semver_excluded(
    scope: &str,
    rows: &[(semver::Version, &str, &str)],
    requirement: &str,
) -> Result<Vec<Excluded>> {
    let requirement = semver::VersionReq::parse(requirement).map_err(|e| {
        Error::Operation(format!(
            "{requirement} is not a Cargo version requirement: {e}"
        ))
    })?;
    let stable: Vec<_> = rows.iter().filter(|(v, _, _)| v.pre.is_empty()).collect();
    let Some((newest, url, checksum)) = stable.iter().max_by(|a, b| a.0.cmp(&b.0)) else {
        return Ok(vec![]);
    };
    let allowed = stable
        .iter()
        .map(|(v, _, _)| v)
        .filter(|v| requirement.matches(v))
        .max();
    if allowed.is_some_and(|a| a >= newest) {
        return Ok(vec![]);
    }
    Ok(vec![Excluded {
        policy: None,
        scope: scope.into(),
        version: newest.to_string(),
        allowed: allowed.map(ToString::to_string),
        url: (*url).into(),
        sha256: (*checksum).into(),
    }])
}

/// Releases of `name` in a sparse index that `requirement` excludes.
pub(crate) fn sparse_excluded(
    index: &str,
    name: &str,
    requirement: &str,
    timeout: u64,
) -> Result<Vec<Excluded>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout))
        .user_agent("depsmith/0.1")
        .build()
        .map_err(|_| Error::Operation("cannot construct HTTP client".into()))?;
    let url = format!("{index}{}", sparse_path(name));
    let response = client
        .get(&url)
        .send()
        .map_err(|e| Error::Operation(format!("request failed: {}", e.without_url())))?;
    if !response.status().is_success() {
        return Err(Error::Operation(format!(
            "{url}: HTTP {}",
            response.status()
        )));
    }
    let text = response
        .text()
        .map_err(|e| Error::Operation(format!("unreadable index response: {}", e.without_url())))?;
    let crates_io = index == CRATES_IO_INDEX;
    let mut rows = vec![];
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let row: serde_json::Value = serde_json::from_str(line)
            .map_err(|e| Error::Operation(format!("unreadable index entry: {e}")))?;
        if row["yanked"] == true {
            continue;
        }
        let (Some(version), Some(checksum)) = (row["vers"].as_str(), row["cksum"].as_str()) else {
            continue;
        };
        let Ok(parsed) = semver::Version::parse(version) else {
            continue;
        };
        let crate_name = row["name"].as_str().unwrap_or(name);
        let artifact = if crates_io {
            format!("{CRATES_IO_DOWNLOAD}/{crate_name}/{version}/download")
        } else {
            format!("{url}#{version}")
        };
        rows.push((parsed, artifact, checksum.to_owned()));
    }
    let borrowed: Vec<_> = rows
        .iter()
        .map(|(v, u, c)| (v.clone(), u.as_str(), c.as_str()))
        .collect();
    let scope = if crates_io { "crates.io" } else { index };
    semver_excluded(scope, &borrowed, requirement)
}

/// Crates `cargo update` left behind because a newer release needs a newer
/// Rust than the workspace's `rust-version`, from its (verbose) report.
fn held_back(report: &str, target: &Target) -> Vec<Suggestion> {
    let pattern = regex::Regex::new(
        r"^\s*\w+\s+(\S+)\s+v(\S+)(?:\s+->\s+v(\S+))?\s+\(available: v([^,()]+), requires Rust ([^)]+)\)",
    )
    .unwrap();
    let mut seen = BTreeSet::new();
    let mut output = vec![];
    for line in report.lines() {
        let Some(c) = pattern.captures(line) else {
            continue;
        };
        let name = &c[1];
        if !seen.insert(name.to_owned()) {
            continue;
        }
        let locked = c.get(3).map_or(&c[2], |m| m.as_str());
        output.push(Suggestion {
            target: target.id.clone(),
            package: name.into(),
            requirement: format!("{locked} (locked)"),
            reason: format!(
                "Held back by the minimum supported Rust version: {name} {} requires Rust {}. Raise rust-version to allow it.",
                &c[4], &c[5]
            ),
            evidence: vec![format!("cargo update: {}", line.trim())],
        });
    }
    output
}

/// Reject cargo older than 1.84, which cannot resolve MSRV-aware.
fn check_version(cargo: &str, stage: &Path, timeout: u64) -> Result<()> {
    let output = run(cargo, &["--version".into()], stage, timeout)?;
    let version = output
        .split_whitespace()
        .nth(1)
        .and_then(|v| semver::Version::parse(v).ok())
        .ok_or_else(|| Error::Operation(format!("unreadable cargo --version: {output}")))?;
    if (version.major, version.minor) < (1, 84) {
        return Err(Error::Invalid(format!(
            "cargo {version} is unsupported: MSRV-aware resolution needs cargo 1.84 or newer; install a newer toolchain or pass --tool cargo=PATH"
        )));
    }
    Ok(())
}

impl Adapter for Cargo {
    fn spec(&self) -> AdapterSpec {
        AdapterSpec {
            manager: "cargo".into(),
            ecosystems: vec!["cargo".into()],
            patterns: vec!["Cargo.toml".into()],
            managed: vec![ManagedFiles {
                manifests: vec!["Cargo.toml".into()],
                inputs: vec!["Cargo.lock".into(), ".cargo/config.toml".into()],
            }],
            skip_dirs: vec!["target".into()],
            tools: vec![ToolSpec {
                name: "cargo".into(),
                default: "cargo".into(),
                tested_versions: vec!["1.98.1".into()],
            }],
            capabilities: Capabilities {
                package_selection: Support::Supported,
                constraint_changes: Support::Unsupported(
                    "change a requirement with --accept NAME or --accept NAME=REQUIREMENT".into(),
                ),
                suggestion_acceptance: Support::Supported,
                git_refresh: Support::Supported,
                cooldown: Support::Unsupported(
                    "a release-age cooldown is not implemented for Cargo".into(),
                ),
                install_validation: Support::NotApplicable,
                lockfile: Support::Supported,
                // Cargo.lock covers every platform.
                platforms: Support::Supported,
            },
        }
    }
    fn detects(&self, _: &Path, content: &str) -> bool {
        content
            .parse::<toml::Value>()
            .is_ok_and(|v| v.get("workspace").is_some() || v.get("package").is_some())
    }
    /// A workspace root, or a package that no enclosing workspace lists as a
    /// member: exactly the manifests that own a `Cargo.lock`.
    fn detects_in(&self, root: &Path, relative: &Path, content: &str) -> bool {
        let Ok(parsed) = content.parse::<toml::Value>() else {
            return false;
        };
        if parsed.get("workspace").is_some() {
            return true;
        }
        let Some(package) = parsed.get("package") else {
            return false;
        };
        if package.get("workspace").is_some() {
            return false;
        }
        let dir = relative.parent().unwrap_or(Path::new(""));
        for ancestor in dir.ancestors().skip(1) {
            let manifest = ancestor.join("Cargo.toml");
            let Ok(Some(workspace)) = read(root, &manifest).map(|v| v.get("workspace").cloned())
            else {
                continue;
            };
            return !members(root, ancestor, &workspace).contains(&relative.to_path_buf());
        }
        true
    }
    fn inventory(&self, root: &Path, target: &Target) -> Result<Vec<Package>> {
        let lock = target.manifest.with_file_name("Cargo.lock");
        match fs::read_to_string(root.join(&lock)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::Invalid(format!(
                "{}: no {} to scan; create it with `cargo generate-lockfile` or `depsmith update`",
                target.id,
                lock.display()
            ))),
            result => lock_inventory(&result?),
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
            let key = crate_key(request);
            let found = declared.iter().find(|(d, _)| {
                crate_key(&d.package) == key
                    || d.location
                        .rsplit_once('.')
                        .is_some_and(|(_, k)| crate_key(k) == key)
            });
            if let Some((declaration, _)) = found {
                selected.insert(request.clone(), declaration.package.clone());
            }
        }
        Ok(selected)
    }
    /// Dependencies from crates.io; other registries are not consulted.
    fn declarations(&self, root: &Path, target: &Target) -> Result<Vec<Declaration>> {
        Ok(entries(root, target)?
            .into_iter()
            .filter(|(_, crates_io)| *crates_io)
            .map(|(d, _)| d)
            .collect())
    }
    fn rewrite(&self, stage: &Path, target: &Target, edits: &[Edit]) -> Result<()> {
        let manifests = manifests(stage, target)?;
        let mut by_file: BTreeMap<&Path, Vec<&Edit>> = BTreeMap::new();
        for edit in edits {
            let file = edit.declaration.file.as_path();
            if !manifests.iter().any(|m| m == file) {
                return Err(Error::Invalid(format!(
                    "{}: {} is not one of this target's manifests",
                    target.id,
                    file.display()
                )));
            }
            by_file.entry(file).or_default().push(edit);
        }
        for (file, edits) in by_file {
            let path = stage.join(file);
            let text = rewrite_manifest(&fs::read_to_string(&path)?, &edits)?;
            fs::write(path, text)?;
        }
        Ok(())
    }
    fn availability(&self, _: &Path, _: &Target) -> Result<AvailabilityConfig> {
        let registry = self
            .registry
            .clone()
            .unwrap_or(RegistryConfig::CratesSparse {
                index: CRATES_IO_INDEX.into(),
            });
        Ok(AvailabilityConfig {
            registries: BTreeMap::from([("cargo".into(), registry)]),
            exclude_newer: None,
        })
    }
    fn prepare(&self, stage: &Path, target: &Target, options: &UpdateOptions) -> Result<Candidate> {
        if options.upgrade {
            return Err(Error::Invalid(
                "--upgrade is not supported by the cargo adapter".into(),
            ));
        }
        let cargo = options.tool("cargo");
        let timeout = options.timeout_seconds;
        check_version(&cargo, stage, timeout)?;
        let manifest = stage.join(&target.manifest);
        let manifest_arg: String = manifest.to_string_lossy().into();
        let lock = target.manifest.with_file_name("Cargo.lock");
        let files = manifests(stage, target)?;
        let sources = files
            .iter()
            .map(|f| fs::read(stage.join(f)))
            .collect::<std::io::Result<Vec<_>>>()?;
        let before_text = fs::read_to_string(stage.join(&lock)).ok();
        let before = before_text
            .as_deref()
            .map(lock_inventory)
            .transpose()?
            .unwrap_or_default();
        // The report is parsed, so never colour it (CARGO_TERM_COLOR may
        // say otherwise).
        let mut args: Vec<String> = vec![
            "update".into(),
            "--verbose".into(),
            "--color".into(),
            "never".into(),
            "--manifest-path".into(),
            manifest_arg.clone(),
        ];
        for package in &options.packages {
            args.extend(["-p".into(), package.clone()]);
        }
        let msrv = [(
            "CARGO_RESOLVER_INCOMPATIBLE_RUST_VERSIONS".to_owned(),
            "fallback".to_owned(),
        )];
        let (_, report) = run_env_output(&cargo, &args, stage, timeout, &msrv)?;
        let after_text = fs::read_to_string(stage.join(&lock))?;
        let after = lock_inventory(&after_text)?;
        let git = |packages: &[Package]| -> BTreeSet<String> {
            packages
                .iter()
                .filter(|p| p.artifact.starts_with("git+"))
                .map(|p| p.artifact.clone())
                .collect()
        };
        if !options.refresh_git && before_text.is_some() && git(&before) != git(&after) {
            return Err(Error::Policy(
                "backend moved Git resolutions; select --refresh-git explicitly".into(),
            ));
        }
        for (file, source) in files.iter().zip(&sources) {
            if fs::read(stage.join(file))? != *source {
                return Err(Error::Policy(format!(
                    "backend unexpectedly changed {}",
                    file.display()
                )));
            }
        }
        run_env_output(
            &cargo,
            &[
                "update".into(),
                "--workspace".into(),
                "--locked".into(),
                "--manifest-path".into(),
                manifest_arg,
            ],
            stage,
            timeout,
            &msrv,
        )?;
        if fs::read_to_string(stage.join(&lock))? != after_text {
            return Err(Error::Operation(
                "lock consistency check changed the candidate".into(),
            ));
        }
        let mut changed = files;
        changed.push(lock);
        Ok(Candidate {
            files: changed,
            suggestions: held_back(&report, target),
            unresolved: vec![],
            before,
            baseline_available: before_text.is_some(),
            after,
            validation: vec![format!("{}: resolved and lock-consistent", target.id)],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparse_index_paths_follow_the_name_length() {
        assert_eq!(sparse_path("a"), "1/a");
        assert_eq!(sparse_path("ab"), "2/ab");
        assert_eq!(sparse_path("Abc"), "3/a/abc");
        assert_eq!(sparse_path("serde_json"), "se/rd/serde_json");
    }

    #[test]
    fn lock_inventory_keeps_sourced_crates() {
        let lock = r#"version = 4
[[package]]
name = "member"
version = "0.1.0"
[[package]]
name = "itoa"
version = "1.0.15"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "x"
[[package]]
name = "g"
version = "0.2.0"
source = "git+https://example.invalid/g.git#0123456789abcdef"
"#;
        let packages: Vec<_> = lock_inventory(lock)
            .unwrap()
            .into_iter()
            .map(|p| (p.name, p.artifact))
            .collect();
        assert_eq!(
            packages,
            [
                (
                    "itoa".into(),
                    "https://crates.io/api/v1/crates/itoa/1.0.15/download".into()
                ),
                (
                    "g".into(),
                    "git+https://example.invalid/g.git#0123456789abcdef".into()
                )
            ]
        );
    }

    #[test]
    fn msrv_hold_backs_are_read_from_the_update_report() {
        let target = Target {
            id: "cargo:Cargo.toml".into(),
            manager: "cargo".into(),
            manifest: "Cargo.toml".into(),
        };
        let report = "    Updating crates.io index\n     Locking 2 packages to latest Rust 1.60 compatible versions\n      Adding once_cell v1.17.0 (available: v1.21.3, requires Rust 1.70)\n    Updating log v0.4.1 -> v0.4.20 (available: v0.4.28, requires Rust 1.61.0)\n   Unchanged memchr v2.5.0 (available: v3.0.0)\n";
        let held: Vec<_> = held_back(report, &target)
            .into_iter()
            .map(|s| (s.package, s.requirement))
            .collect();
        assert_eq!(
            held,
            [
                ("once_cell".into(), "1.17.0 (locked)".into()),
                ("log".into(), "0.4.20 (locked)".into())
            ]
        );
    }

    #[test]
    fn semver_evidence_cites_the_newest_excluded_release() {
        let rows: Vec<(semver::Version, &str, &str)> =
            ["1.0.100", "1.0.200", "2.0.0", "3.0.0-rc.1"]
                .iter()
                .map(|v| (semver::Version::parse(v).unwrap(), "u", "c"))
                .collect();
        let excluded = semver_excluded("crates.io", &rows, "1.0.100").unwrap();
        assert_eq!(excluded[0].version, "2.0.0");
        assert_eq!(excluded[0].allowed.as_deref(), Some("1.0.200"));
        assert!(semver_excluded("crates.io", &rows, ">=1")
            .unwrap()
            .is_empty());
    }
}

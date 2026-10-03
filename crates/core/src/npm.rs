//! The npm adapter: each `package-lock.json` owner (a workspace root or a
//! standalone package) is a target whose declarations are the dependency
//! objects of its `package.json` files. npm resolves in the stage without
//! running scripts; Git pins are kept unless `--refresh-git` is selected, and
//! `--cooldown-days` becomes npm's `--before`.
use crate::{
    adapter::{
        edits_by_manifest, undeclared, Adapter, AdapterSpec, Candidate, Capabilities, LockCheck,
        ManagedFiles, Support, ToolSpec,
    },
    constraint::Excluded,
    constraints::{AvailabilityConfig, Declaration, Edit, RegistryConfig},
    process::run_env,
    Error, Package, Result, Target, UpdateOptions,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    ops::Range,
    path::{Path, PathBuf},
};

/// The public npm registry; packages from it are scanned as `pkg:npm`.
pub(crate) const NPM_REGISTRY: &str = "https://registry.npmjs.org/";
const DEPENDENCY_OBJECTS: [&str; 3] = ["dependencies", "devDependencies", "optionalDependencies"];
const LOCKS: [&str; 2] = ["package-lock.json", "npm-shrinkwrap.json"];

/// The npm adapter. Availability evidence comes from the project's registry
/// unless `registry` replaces it (for tests and mirrors).
#[derive(Default)]
pub struct Npm {
    /// Registry consulted for newer releases instead of the project's.
    pub registry: Option<RegistryConfig>,
}

fn parse(text: &str, path: &Path) -> Result<Value> {
    serde_json::from_str(text)
        .map_err(|e| Error::Invalid(format!("invalid manifest {}: {e}", path.display())))
}

fn read(root: &Path, relative: &Path) -> Result<Value> {
    parse(&fs::read_to_string(root.join(relative))?, relative)
}

/// `workspaces` as an array, or the `packages` of its object form.
fn workspace_patterns(manifest: &Value) -> Vec<&str> {
    let workspaces = manifest.get("workspaces");
    workspaces
        .and_then(Value::as_array)
        .or_else(|| workspaces?.get("packages")?.as_array())
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

/// Member manifests of the workspace whose root manifest is in `dir`.
fn members(root: &Path, dir: &Path, manifest: &Value) -> Vec<PathBuf> {
    let mut output = vec![];
    for pattern in workspace_patterns(manifest) {
        for member in crate::working_tree::expand(root, dir, pattern) {
            let path = member.join("package.json");
            if member != dir && root.join(&path).is_file() && !output.contains(&path) {
                output.push(path);
            }
        }
    }
    output
}

/// The manifests of `target`: its own, then its workspace members.
fn manifests(root: &Path, target: &Target) -> Result<Vec<PathBuf>> {
    let parsed = read(root, &target.manifest)?;
    let dir = target.manifest.parent().unwrap_or(Path::new(""));
    let mut output = vec![target.manifest.clone()];
    output.extend(members(root, dir, &parsed));
    Ok(output)
}

/// Whether a dependency spec resolves from the registry (a version range),
/// as opposed to Git, a path, a URL, a workspace or an alias.
fn from_registry(spec: &str) -> bool {
    let spec = spec.trim();
    !(spec.contains(':') || spec.contains('/') || spec.starts_with('.'))
}

/// Every dependency of the target as (declaration, resolved from the
/// registry).
fn entries(root: &Path, target: &Target) -> Result<Vec<(Declaration, bool)>> {
    let mut output = vec![];
    for manifest in manifests(root, target)? {
        let parsed = read(root, &manifest)?;
        for object in DEPENDENCY_OBJECTS {
            for (name, spec) in parsed
                .get(object)
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
            {
                let Some(spec) = spec.as_str() else {
                    continue;
                };
                let registry = from_registry(spec);
                output.push((
                    Declaration {
                        ecosystem: "npm".into(),
                        package: name.clone(),
                        requirement: if registry {
                            spec.trim().into()
                        } else {
                            String::new()
                        },
                        file: manifest.clone(),
                        location: format!("{object}.{name}"),
                    },
                    registry,
                ));
            }
        }
    }
    Ok(output)
}

/// The byte range of the string value of `object.name` in the JSON `text`
/// (inside the quotes), found by scanning so that the rest of the file keeps
/// its formatting.
fn value_span(text: &str, object: &str, name: &str) -> Option<Range<usize>> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut in_object = false;
    let mut pending_key: Option<(Range<usize>, usize)> = None;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                let start = index + 1;
                index += 1;
                while index < bytes.len() && bytes[index] != b'"' {
                    index += if bytes[index] == b'\\' { 2 } else { 1 };
                }
                let string = start..index.min(bytes.len());
                // A value following `key:`?
                if let Some((key, key_depth)) = pending_key.take() {
                    if in_object && key_depth == 2 && &text[key] == name {
                        return Some(string);
                    }
                } else {
                    // A key when followed by `:`.
                    let mut next = index + 1;
                    while next < bytes.len() && bytes[next].is_ascii_whitespace() {
                        next += 1;
                    }
                    if next < bytes.len() && bytes[next] == b':' {
                        if depth == 1 && &text[string.clone()] == object {
                            let mut open = next + 1;
                            while open < bytes.len() && bytes[open].is_ascii_whitespace() {
                                open += 1;
                            }
                            in_object = open < bytes.len() && bytes[open] == b'{';
                        }
                        pending_key = Some((string, depth));
                        index = next;
                    }
                }
            }
            b'{' | b'[' => {
                depth += 1;
                pending_key = None;
            }
            b'}' | b']' => {
                if depth == 2 && bytes[index] == b'}' {
                    in_object = false;
                }
                depth = depth.saturating_sub(1);
                pending_key = None;
            }
            b',' => pending_key = None,
            _ => {}
        }
        index += 1;
    }
    None
}

/// Apply the edits of one manifest's text, keeping everything else as is.
fn rewrite_manifest(text: &str, edits: &[&Edit]) -> Result<String> {
    let mut text = text.to_owned();
    for edit in edits {
        let d = &edit.declaration;
        let (object, name) = d.location.split_once('.').ok_or_else(|| undeclared(d))?;
        let span = value_span(&text, object, name).ok_or_else(|| undeclared(d))?;
        if text[span.clone()].trim() != d.requirement {
            return Err(undeclared(d));
        }
        let escaped = serde_json::to_string(&edit.requirement)
            .map_err(|e| Error::Invalid(format!("unwritable requirement: {e}")))?;
        text.replace_range(span, &escaped[1..escaped.len() - 1]);
    }
    Ok(text)
}

/// Resolved packages of a `package-lock.json` (lockfileVersion 2 or 3). The
/// root, workspace members and other links are the project itself and are
/// omitted; a package's artifact is its `resolved` tarball or Git URL.
///
/// # Errors
///
/// Returns [`Error::Invalid`] when the lock is not JSON or has an unsupported
/// `lockfileVersion`.
pub fn lock_inventory(text: &str) -> Result<Vec<Package>> {
    let parsed: Value = serde_json::from_str(text)
        .map_err(|e| Error::Invalid(format!("invalid package-lock.json: {e}")))?;
    let version = parsed.get("lockfileVersion").and_then(Value::as_u64);
    if !matches!(version, Some(2 | 3)) {
        return Err(Error::Invalid(format!(
            "unsupported package-lock.json lockfileVersion {}; supported: 2 and 3 (npm 7 or newer)",
            version.map_or_else(|| "(missing)".into(), |v| v.to_string())
        )));
    }
    let mut packages = vec![];
    for (key, entry) in parsed
        .get("packages")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let Some((_, name)) = key.rsplit_once("node_modules/") else {
            continue;
        };
        if entry.get("link").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let field = |f: &str| entry.get(f).and_then(Value::as_str);
        let (Some(version), Some(resolved)) = (field("version"), field("resolved")) else {
            continue;
        };
        packages.push(Package {
            ecosystem: "npm".into(),
            name: field("name").unwrap_or(name).into(),
            version: version.into(),
            artifact: resolved.into(),
            platform: "any".into(),
        });
    }
    Ok(packages)
}

/// The registry of the project at `dir`: `registry=` in its `.npmrc`, else
/// the public registry. Credentials in the URL are never kept.
fn registry_url(root: &Path, dir: &Path) -> String {
    let configured = fs::read_to_string(root.join(dir).join(".npmrc"))
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                let (key, value) = line.split_once('=')?;
                (key.trim() == "registry").then(|| value.trim().to_owned())
            })
        })
        .and_then(|url| {
            let mut url = reqwest::Url::parse(&url).ok()?;
            url.set_username("").ok()?;
            url.set_password(None).ok()?;
            Some(url.to_string())
        });
    let mut url = configured.unwrap_or_else(|| NPM_REGISTRY.into());
    if !url.ends_with('/') {
        url.push('/');
    }
    url
}

/// Newest stable release excluded by `requirement` among `(version, url,
/// integrity)` rows; empty when the requirement admits the newest.
pub(crate) fn npm_excluded(
    scope: &str,
    rows: &[(nodejs_semver::Version, &str, &str)],
    requirement: &str,
) -> Result<Vec<Excluded>> {
    let range = nodejs_semver::Range::parse(requirement)
        .map_err(|e| Error::Operation(format!("{requirement} is not an npm version range: {e}")))?;
    let stable: Vec<_> = rows.iter().filter(|(v, _, _)| !v.is_prerelease()).collect();
    let Some((newest, url, integrity)) = stable.iter().max_by(|a, b| a.0.cmp(&b.0)) else {
        return Ok(vec![]);
    };
    let allowed = stable
        .iter()
        .map(|(v, _, _)| v)
        .filter(|v| range.satisfies(v))
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
        sha256: (*integrity).into(),
    }])
}

/// Releases of `name` on the registry at `url` (ending in `/`) that
/// `requirement` excludes, from its abbreviated metadata: the newest stable
/// release with its tarball and integrity, and the newest one allowed.
/// Deprecated releases are not cited.
///
/// # Errors
///
/// Returns [`Error::Operation`] when the registry cannot be read or the
/// requirement is not an npm range.
pub(crate) fn registry_excluded(
    url: &str,
    name: &str,
    requirement: &str,
    timeout: u64,
) -> Result<Vec<Excluded>> {
    let client = crate::http::client(timeout)?;
    let document = format!("{url}{}", name.replacen('/', "%2f", 1));
    let response = client
        .get(&document)
        .header("Accept", "application/vnd.npm.install-v1+json")
        .send()
        .map_err(|e| Error::Operation(format!("request failed: {}", e.without_url())))?;
    if !response.status().is_success() {
        return Err(Error::Operation(format!(
            "{document}: HTTP {}",
            response.status()
        )));
    }
    let metadata: Value = response.json().map_err(|e| {
        Error::Operation(format!("unreadable registry response: {}", e.without_url()))
    })?;
    let mut rows = vec![];
    for (version, release) in metadata
        .get("versions")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        if release.get("deprecated").is_some() {
            continue;
        }
        let Ok(parsed) = nodejs_semver::Version::parse(version) else {
            continue;
        };
        let dist = release.get("dist");
        let field = |f: &str| {
            dist.and_then(|d| d.get(f))
                .and_then(Value::as_str)
                .unwrap_or_default()
        };
        rows.push((
            parsed,
            field("tarball").to_owned(),
            field("integrity").to_owned(),
        ));
    }
    let borrowed: Vec<_> = rows
        .iter()
        .map(|(v, u, i)| (v.clone(), u.as_str(), i.as_str()))
        .collect();
    let scope = reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_else(|| url.to_owned());
    npm_excluded(&scope, &borrowed, requirement)
}

/// The `--before` cutoff for a cooldown of `days`, as RFC 3339 UTC.
fn before(days: u32) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let cutoff = now.saturating_sub(u64::from(days) * 86_400);
    let (days_since, seconds) = (cutoff / 86_400, cutoff % 86_400);
    // Civil date from days since 1970-01-01 (Hinnant's algorithm).
    let z = days_since as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

impl Adapter for Npm {
    fn spec(&self) -> AdapterSpec {
        AdapterSpec {
            manager: "npm".into(),
            ecosystems: vec!["npm".into()],
            patterns: vec!["package.json".into()],
            managed: vec![ManagedFiles {
                manifests: vec!["package.json".into()],
                inputs: vec![
                    "package-lock.json".into(),
                    "npm-shrinkwrap.json".into(),
                    ".npmrc".into(),
                ],
            }],
            skip_dirs: vec!["node_modules".into()],
            tools: vec![ToolSpec {
                name: "npm".into(),
                default: "npm".into(),
                tested_versions: vec!["11.19.0".into()],
                downloads: crate::provision::pinned("npm"),
            }],
            capabilities: Capabilities {
                package_selection: Support::Supported,
                constraint_changes: Support::Unsupported(
                    "change a requirement with --accept NAME or --accept NAME=REQUIREMENT".into(),
                ),
                suggestion_acceptance: Support::Supported,
                git_refresh: Support::Supported,
                cooldown: Support::Supported,
                install_validation: Support::Unsupported(
                    "installing the candidate is not implemented for npm".into(),
                ),
                lockfile: Support::Supported,
                // package-lock.json records every platform's optional packages.
                platforms: Support::Supported,
            },
        }
    }
    fn detects(&self, _: &Path, content: &str) -> bool {
        serde_json::from_str::<Value>(content).is_ok_and(|v| v.is_object())
    }
    /// A package.json with an npm lock beside it, and not a member of an
    /// enclosing npm workspace: exactly the packages that own the lock.
    fn detects_in(&self, root: &Path, relative: &Path, content: &str) -> bool {
        if !self.detects(relative, content) {
            return false;
        }
        let dir = relative.parent().unwrap_or(Path::new(""));
        if !LOCKS.iter().any(|lock| root.join(dir).join(lock).is_file()) {
            return false;
        }
        for ancestor in dir.ancestors().skip(1) {
            let Ok(manifest) = read(root, &ancestor.join("package.json")) else {
                continue;
            };
            if !workspace_patterns(&manifest).is_empty() {
                return !members(root, ancestor, &manifest).contains(&relative.to_path_buf());
            }
        }
        true
    }
    fn inventory(&self, root: &Path, target: &Target) -> Result<Vec<Package>> {
        let dir = target.manifest.parent().unwrap_or(Path::new(""));
        let lock = LOCKS
            .iter()
            .map(|l| dir.join(l))
            .find(|l| root.join(l).is_file());
        match lock {
            None => Err(Error::Invalid(format!(
                "{}: no package-lock.json to scan; create it with `npm install --package-lock-only` or `depsmith update`",
                target.id
            ))),
            Some(lock) => lock_inventory(&fs::read_to_string(root.join(lock))?),
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
            if let Some((declaration, _)) = declared.iter().find(|(d, _)| d.package == *request) {
                selected.insert(request.clone(), declaration.package.clone());
            }
        }
        Ok(selected)
    }
    /// Registry dependencies; Git, path, URL, workspace and alias specs are
    /// not consulted.
    fn declarations(&self, root: &Path, target: &Target) -> Result<Vec<Declaration>> {
        Ok(entries(root, target)?
            .into_iter()
            .filter(|(_, registry)| *registry)
            .map(|(d, _)| d)
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
    fn availability(&self, root: &Path, target: &Target) -> Result<AvailabilityConfig> {
        let dir = target.manifest.parent().unwrap_or(Path::new(""));
        let registry = self
            .registry
            .clone()
            .unwrap_or_else(|| RegistryConfig::NpmRegistry {
                url: registry_url(root, dir),
            });
        Ok(AvailabilityConfig {
            registries: BTreeMap::from([("npm".into(), registry)]),
            exclude_newer: None,
        })
    }
    fn prepare(&self, stage: &Path, target: &Target, options: &UpdateOptions) -> Result<Candidate> {
        if options.upgrade {
            return Err(Error::Invalid(
                "--upgrade is not supported by the npm adapter".into(),
            ));
        }
        let npm = options.tool("npm");
        let env = options.tool_env("npm");
        let timeout = options.timeout_seconds;
        let dir_relative = target.manifest.parent().unwrap_or(Path::new(""));
        let dir = stage.join(dir_relative);
        let lock = LOCKS
            .iter()
            .map(|l| dir_relative.join(l))
            .find(|l| stage.join(l).is_file())
            .unwrap_or_else(|| dir_relative.join("package-lock.json"));
        let check = LockCheck::take(stage, manifests(stage, target)?, lock, lock_inventory)?;
        // Never run package scripts; no audit or funding requests.
        let mut quiet: Vec<String> = [
            "--package-lock-only",
            "--ignore-scripts",
            "--no-audit",
            "--no-fund",
        ]
        .map(str::to_owned)
        .into();
        if let Some(days) = options.cooldown_days {
            quiet.push(format!("--before={}", before(days)));
        }
        let mut args = vec!["update".to_owned()];
        if !options.packages.is_empty() {
            args.extend(options.packages.iter().cloned());
        } else if !options.refresh_git && check.has_git_pins() {
            // A plain update would move Git pins: update every other package.
            let names: BTreeSet<&str> = check
                .before()
                .iter()
                .filter(|p| !p.artifact.starts_with("git+"))
                .map(|p| p.name.as_str())
                .collect();
            args.extend(names.into_iter().map(str::to_owned));
        }
        args.extend(quiet.iter().cloned());
        run_env(&npm, &args, &dir, timeout, env)?;
        let resolved = check.resolved(stage, options.refresh_git)?;
        let mut consistency = vec!["install".to_owned()];
        consistency.extend(quiet);
        run_env(&npm, &consistency, &dir, timeout, env)?;
        check.candidate(stage, target, &resolved, vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_evidence_cites_the_newest_excluded_release() {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
        let body = serde_json::json!({
            "name": "ms",
            "versions": {
                "2.0.0": {"dist": {"tarball": "https://r/ms-2.0.0.tgz", "integrity": "sha512-a"}},
                "2.1.3": {"dist": {"tarball": "https://r/ms-2.1.3.tgz", "integrity": "sha512-b"}},
                "3.0.0-canary.1": {"dist": {"tarball": "https://r/ms-3c.tgz", "integrity": "sha512-c"}},
                "2.2.0": {"deprecated": "broken", "dist": {"tarball": "https://r/ms-2.2.0.tgz", "integrity": "sha512-d"}}
            }
        })
        .to_string();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(&stream);
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                    line.clear();
                }
                let mut stream = &stream;
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        let excluded = registry_excluded(&base, "ms", "2.0.0", 30).unwrap();
        assert_eq!(excluded.len(), 1);
        assert_eq!(excluded[0].version, "2.1.3");
        assert_eq!(excluded[0].allowed.as_deref(), Some("2.0.0"));
        assert_eq!(excluded[0].sha256, "sha512-b");
        assert!(registry_excluded(&base, "ms", "^2.0.0", 30)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn values_are_found_by_scanning_and_formatting_is_kept() {
        let text = "{\n  \"name\": \"x\",\n  \"dependencies\": {\n    \"ms\": \"2.0.0\",\n    \"debug\" : \"^3.0.0\"\n  },\n  \"devDependencies\": { \"ms\": \"~1.0.0\" }\n}\n";
        assert_eq!(
            &text[value_span(text, "dependencies", "ms").unwrap()],
            "2.0.0"
        );
        assert_eq!(
            &text[value_span(text, "dependencies", "debug").unwrap()],
            "^3.0.0"
        );
        assert_eq!(
            &text[value_span(text, "devDependencies", "ms").unwrap()],
            "~1.0.0"
        );
        assert!(value_span(text, "dependencies", "absent").is_none());
        assert!(value_span(text, "peerDependencies", "ms").is_none());
    }

    #[test]
    fn cooldown_cutoffs_are_rfc3339() {
        let cutoff = before(7);
        assert!(
            regex::Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$")
                .unwrap()
                .is_match(&cutoff),
            "{cutoff}"
        );
        assert!(before(7) < before(0));
    }

    #[test]
    fn registry_specs_are_ranges() {
        for spec in ["^1.2.3", "~1", "1.x", ">=1 <2", "*", "latest"] {
            assert!(from_registry(spec), "{spec}");
        }
        for spec in [
            "git+https://x/y.git",
            "github:a/b",
            "a/b",
            "file:../x",
            "./x",
            "npm:ms@2",
            "workspace:*",
            "https://x/y.tgz",
        ] {
            assert!(!from_registry(spec), "{spec}");
        }
    }
}

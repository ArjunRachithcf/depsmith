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

/// `text` without the UTF-8 byte order mark some Windows editors write,
/// which npm accepts but JSON parsers do not.
fn without_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

fn parse(text: &str, path: &Path) -> Result<Value> {
    serde_json::from_str(without_bom(text))
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

/// Member manifests of the workspace whose root manifest is in `dir`; a
/// `!pattern` removes the members matched so far.
fn members(root: &Path, dir: &Path, manifest: &Value) -> Vec<PathBuf> {
    let mut output: Vec<PathBuf> = vec![];
    for pattern in workspace_patterns(manifest) {
        if let Some(negated) = pattern.strip_prefix('!') {
            let excluded: Vec<_> = crate::working_tree::expand(root, dir, negated)
                .into_iter()
                .map(|member| member.join("package.json"))
                .collect();
            output.retain(|path| !excluded.contains(path));
            continue;
        }
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
    // Like JSON.parse and serde, the last of duplicate keys wins.
    let mut found = None;
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
                        found = Some(string);
                    }
                } else {
                    // A key when followed by `:`.
                    let mut next = index + 1;
                    while next < bytes.len() && bytes[next].is_ascii_whitespace() {
                        next += 1;
                    }
                    if next < bytes.len() && bytes[next] == b':' {
                        if depth == 1 && &text[string.clone()] == object {
                            found = None;
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
    found
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
/// omitted; a package's artifact is its `resolved` tarball or Git URL, else
/// its integrity.
///
/// # Errors
///
/// Returns [`Error::Invalid`] when the lock is not JSON or has an unsupported
/// `lockfileVersion`.
pub fn lock_inventory(text: &str) -> Result<Vec<Package>> {
    let parsed: Value = serde_json::from_str(without_bom(text))
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
        let Some(version) = field("version") else {
            continue;
        };
        // With `omit-lockfile-registry-resolved`, registry packages record
        // only their integrity; their registry is unknown, so they are not
        // scanned by registry identity.
        let Some(resolved) = field("resolved").or_else(|| field("integrity")) else {
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

/// `${NAME}` references replaced from the environment, as npm does in
/// configuration keys and values: an unset `${NAME?}` becomes empty, an unset
/// `${NAME}` stays as written, and a backslash escapes the reference.
fn expand_env(text: &str) -> String {
    let mut output = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("${") {
        let before = &rest[..at];
        let body = &rest[at + 2..];
        let Some(end) = body.find('}') else {
            break;
        };
        let inner = &body[..end];
        let (name, optional) = inner
            .strip_suffix('?')
            .map_or((inner, false), |name| (name, true));
        if name.is_empty() || name.contains(['$', '{', '?']) {
            output.push_str(&rest[..at + 2]);
            rest = body;
            continue;
        }
        let escapes = before.len() - before.trim_end_matches('\\').len();
        output.push_str(&before[..before.len() - escapes]);
        output.push_str(&"\\".repeat(escapes / 2));
        if escapes % 2 == 1 {
            output.push_str(&rest[at..at + 3 + end]);
        } else {
            match std::env::var(name) {
                Ok(value) => output.push_str(&value),
                Err(_) if optional => {}
                Err(_) => output.push_str(&rest[at..at + 3 + end]),
            }
        }
        rest = &body[end + 1..];
    }
    output.push_str(rest);
    output
}

/// The `key = value` settings of an npm configuration file as npm reads
/// them: comments and sections skipped, quotes removed and, when `expand`,
/// environment references replaced. A missing file has none.
fn npmrc_settings(path: &Path, expand: bool) -> Result<Vec<(String, String)>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(error.into()),
    };
    let mut settings = vec![];
    for line in without_bom(&text).lines().map(str::trim) {
        if line.is_empty() || line.starts_with([';', '#', '[']) {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        let value = if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
            serde_json::from_str(value).unwrap_or_else(|_| value[1..value.len() - 1].to_owned())
        } else if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
            value[1..value.len() - 1].to_owned()
        } else {
            value.to_owned()
        };
        let key = key.trim().to_owned();
        settings.push(if expand {
            (expand_env(&key), expand_env(&value))
        } else {
            (key, value)
        });
    }
    Ok(settings)
}

/// The user's npm configuration file: `NPM_CONFIG_USERCONFIG`, else
/// `~/.npmrc`.
fn user_npmrc() -> Option<PathBuf> {
    env_setting("userconfig")
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|home| home.join(".npmrc")))
}

/// An `npm_config_<key>` environment variable (any case), which overrides
/// configuration files.
fn env_setting(key: &str) -> Option<String> {
    let name = format!("npm_config_{key}");
    std::env::vars()
        .find(|(k, v)| k.eq_ignore_ascii_case(&name) && !v.is_empty())
        .map(|(_, v)| v)
}

/// A registry URL as npm uses it: without credentials and ending in `/`.
/// One still naming `${VAR}` (unset, or in a repository's `.npmrc`, which is
/// never expanded) is refused.
fn registry_base(key: &str, value: &str) -> Result<String> {
    let invalid = || {
        Error::Invalid(format!(
            "npm configuration {key} = {value:?} is not a registry URL; \
             environment references are expanded only in the user's .npmrc"
        ))
    };
    if value.contains("${") {
        return Err(invalid());
    }
    let mut url = reqwest::Url::parse(value).map_err(|_| invalid())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(invalid());
    }
    url.set_username("").map_err(|()| invalid())?;
    url.set_password(None).map_err(|()| invalid())?;
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    Ok(url.to_string())
}

/// The registries npm would choose with the `project` and `user`
/// configuration files: the environment, then the project's, then the
/// user's, else the public registry; `@scope:registry` settings route scoped
/// packages.
///
/// The repository controls the project file, so it may choose registries but
/// never what secrets reach them: its `${VAR}` references are not expanded
/// (a registry naming one is not a URL and fails), and credentials are read
/// only from the user's file.
fn registry_from(project: &Path, user: Option<PathBuf>) -> Result<RegistryConfig> {
    let mut settings = BTreeMap::new();
    if let Some(user) = &user {
        settings.extend(npmrc_settings(user, true)?);
    }
    settings.extend(npmrc_settings(project, false)?);
    let url = match env_setting("registry").or_else(|| settings.get("registry").cloned()) {
        Some(value) => registry_base("registry", &value)?,
        None => NPM_REGISTRY.into(),
    };
    let mut scopes = BTreeMap::new();
    for (key, value) in &settings {
        if let Some(scope) = key.strip_suffix(":registry").filter(|s| s.starts_with('@')) {
            let value = env_setting(key).unwrap_or_else(|| value.clone());
            scopes.insert(scope.to_owned(), registry_base(key, &value)?);
        }
    }
    Ok(RegistryConfig::NpmRegistry {
        url,
        scopes,
        npmrc: user.into_iter().collect(),
    })
}

/// The `Authorization` header npm would send to `url`: the `_authToken` or
/// `_auth` of the longest `//host/path/:` prefix of it in the user's
/// `npmrc` files (never a repository's).
fn authorization(npmrc: &[PathBuf], url: &str) -> Result<Option<String>> {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return Ok(None);
    };
    let host = parsed.host_str().unwrap_or_default();
    let host = match parsed.port() {
        Some(port) => format!("//{host}:{port}"),
        None => format!("//{host}"),
    };
    let mut settings = BTreeMap::new();
    for file in npmrc.iter().rev() {
        settings.extend(npmrc_settings(file, true)?);
    }
    let mut path = parsed.path().to_owned();
    loop {
        // npm accepts each prefix with and without its trailing slash.
        let prefix = format!("{host}{path}");
        let keys = [prefix.as_str(), prefix.trim_end_matches('/')];
        let setting = |name: &str| {
            keys.iter()
                .find_map(|k| settings.get(&format!("{k}:{name}")))
        };
        if let Some(token) = setting("_authToken") {
            return Ok(Some(format!("Bearer {token}")));
        }
        if let Some(basic) = setting("_auth") {
            return Ok(Some(format!("Basic {basic}")));
        }
        let Some(cut) = path.trim_end_matches('/').rfind('/') else {
            return Ok(None);
        };
        path.truncate(cut + 1);
    }
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
        // npm's Subresource Integrity string carries its own algorithm.
        digest: (*integrity).into(),
    }])
}

/// Releases of `name` on its registry that `requirement` excludes: the
/// newest stable release with its tarball and integrity, and the newest one
/// allowed. Scoped packages use their scope's registry, with the credentials
/// npm would send. Deprecated releases are not cited, and with a `cutoff`
/// (milliseconds since the epoch) neither are releases published after it,
/// read from the full metadata since the abbreviated form has no times.
///
/// # Errors
///
/// Returns [`Error::Operation`] when the registry cannot be read or the
/// requirement is not an npm range.
/// The registries of a [`RegistryConfig::NpmRegistry`].
pub(crate) struct Registries<'a> {
    pub url: &'a str,
    pub scopes: &'a BTreeMap<String, String>,
    pub npmrc: &'a [PathBuf],
}

pub(crate) fn registry_excluded(
    registry: Registries<'_>,
    name: &str,
    requirement: &str,
    timeout: u64,
    cutoff: Option<i64>,
) -> Result<Vec<Excluded>> {
    let url = name
        .split_once('/')
        .and_then(|(scope, _)| registry.scopes.get(scope))
        .map_or(registry.url, String::as_str);
    let client = crate::http::client(timeout)?;
    let document = format!("{url}{}", name.replacen('/', "%2f", 1));
    let mut request = client.get(&document).header(
        "Accept",
        if cutoff.is_some() {
            "application/json"
        } else {
            "application/vnd.npm.install-v1+json"
        },
    );
    if let Some(authorization) = authorization(registry.npmrc, url)? {
        request = request.header("Authorization", authorization);
    }
    let response = request
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
    let published = |version: &str| {
        metadata
            .get("time")
            .and_then(|t| t.get(version))
            .and_then(Value::as_str)
            .and_then(crate::cutoff::timestamp)
    };
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
        if cutoff.is_some_and(|cutoff| published(version).is_none_or(|time| time > cutoff)) {
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
        // Releases published before npm 5 have only a SHA-1 `shasum`.
        let digest = match (field("integrity"), field("shasum")) {
            ("", "") => "unknown".to_owned(),
            ("", shasum) => format!("sha1:{shasum}"),
            (integrity, _) => integrity.to_owned(),
        };
        rows.push((parsed, field("tarball").to_owned(), digest));
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

/// cmd.exe's command-line limit (8191) for the names and options of an
/// `npm.cmd` call on Windows, less headroom for the `cmd.exe /c` wrapper and
/// the `"%NODE_EXE%" "%NPM_CLI_JS%" %*` line npm.cmd expands them into.
const WINDOWS_COMMAND_LIMIT: usize = 8191 - 512;

/// Names for an `npm update` that keeps Git pins: every package of the lock
/// not resolved from Git, by its `node_modules` folder name (which npm
/// matches, so aliases work). When a command naming them all, with `used`
/// characters of program and options, would exceed `limit`, only the
/// `declared` registry dependencies are named, with a note saying so.
///
/// # Errors
///
/// Returns [`Error::Invalid`] when even the declared names are too many.
fn update_names(
    lock: &str,
    declared: &[String],
    used: usize,
    limit: usize,
) -> Result<(Vec<String>, Option<String>)> {
    let parsed: Value = serde_json::from_str(without_bom(lock))
        .map_err(|e| Error::Invalid(format!("invalid package-lock.json: {e}")))?;
    let mut names = BTreeSet::new();
    for (key, entry) in parsed
        .get("packages")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let Some((_, folder)) = key.rsplit_once("node_modules/") else {
            continue;
        };
        let resolved = entry.get("resolved").and_then(Value::as_str);
        if entry.get("link").and_then(Value::as_bool) != Some(true)
            && !resolved.is_some_and(|r| r.starts_with("git+"))
        {
            names.insert(folder.to_owned());
        }
    }
    let length =
        |names: &mut dyn Iterator<Item = &String>| used + names.map(|n| n.len() + 1).sum::<usize>();
    if length(&mut names.iter()) <= limit {
        return Ok((names.into_iter().collect(), None));
    }
    let declared: BTreeSet<String> = declared.iter().cloned().collect();
    if length(&mut declared.iter()) <= limit {
        let count = declared.len();
        return Ok((
            declared.into_iter().collect(),
            Some(format!(
                "npm update named only the {} declared dependencies: naming all {} packages \
                 that are not Git pins exceeds the {limit}-character command line, so \
                 transitive packages moved only where a declared one required it",
                count,
                names.len()
            )),
        ));
    }
    Err(Error::Invalid(format!(
        "keeping Git pins needs an npm update naming {} packages, longer than the \
         {limit}-character command line; select packages with --package, or let \
         Git pins move with --refresh-git",
        declared.len()
    )))
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
        serde_json::from_str::<Value>(without_bom(content)).is_ok_and(|v| v.is_object())
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
        let registry = match &self.registry {
            Some(registry) => registry.clone(),
            None => registry_from(&root.join(dir).join(".npmrc"), user_npmrc())?,
        };
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
        let check = LockCheck::take(
            stage,
            manifests(stage, target)?,
            lock.clone(),
            lock_inventory,
        )?;
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
        let mut note = None;
        if !options.packages.is_empty() {
            args.extend(options.packages.iter().cloned());
        } else if !options.refresh_git && check.has_git_pins() {
            // A plain update would move Git pins: update every other package.
            let declared: Vec<String> = entries(stage, target)?
                .into_iter()
                .filter(|(_, registry)| *registry)
                .map(|(d, _)| d.package)
                .collect();
            // The program, ` update` and each option with its separator.
            let used =
                npm.len() + " update".len() + quiet.iter().map(|a| a.len() + 1).sum::<usize>();
            let limit = if cfg!(windows) {
                WINDOWS_COMMAND_LIMIT
            } else {
                usize::MAX
            };
            let lock_text = fs::read_to_string(stage.join(&lock))?;
            let (names, narrowed) = update_names(&lock_text, &declared, used, limit)?;
            args.extend(names);
            note = narrowed;
        }
        args.extend(quiet.iter().cloned());
        run_env(&npm, &args, &dir, timeout, env)?;
        let resolved = check.resolved(stage, options.refresh_git)?;
        let mut consistency = vec!["install".to_owned()];
        consistency.extend(quiet);
        run_env(&npm, &consistency, &dir, timeout, env)?;
        let mut candidate = check.candidate(stage, target, &resolved, vec![])?;
        candidate.validation.extend(note);
        Ok(candidate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A registry serving `body` for every request; returns its URL and the
    /// request heads it received.
    fn registry(body: Value) -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
        let body = body.to_string();
        let (heads, received) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(&stream);
                let (mut line, mut head) = (String::new(), String::new());
                while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                    head.push_str(&line.to_ascii_lowercase());
                    line.clear();
                }
                let _ = heads.send(head);
                let mut stream = &stream;
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        (base, received)
    }

    #[test]
    fn registry_evidence_cites_the_newest_excluded_release() {
        let (base, heads) = registry(serde_json::json!({
            "name": "ms",
            "versions": {
                "2.0.0": {"dist": {"tarball": "https://r/ms-2.0.0.tgz", "integrity": "sha512-a"}},
                "2.1.3": {"dist": {"tarball": "https://r/ms-2.1.3.tgz", "integrity": "sha512-b"}},
                "3.0.0-canary.1": {"dist": {"tarball": "https://r/ms-3c.tgz", "integrity": "sha512-c"}},
                "2.2.0": {"deprecated": "broken", "dist": {"tarball": "https://r/ms-2.2.0.tgz", "integrity": "sha512-d"}}
            }
        }));
        let none = BTreeMap::new();
        let excluded = registry_excluded(
            Registries {
                url: &base,
                scopes: &none,
                npmrc: &[],
            },
            "ms",
            "2.0.0",
            30,
            None,
        )
        .unwrap();
        assert_eq!(excluded.len(), 1);
        assert_eq!(excluded[0].version, "2.1.3");
        assert_eq!(excluded[0].allowed.as_deref(), Some("2.0.0"));
        // npm's integrity is SHA-512 and is cited with its own label.
        assert_eq!(excluded[0].digest, "sha512-b");
        assert!(excluded[0]
            .to_string()
            .ends_with("https://r/ms-2.1.3.tgz sha512-b"));
        let head = heads.recv().unwrap();
        assert!(
            head.contains("application/vnd.npm.install-v1+json"),
            "{head}"
        );
        assert!(!head.contains("authorization"), "{head}");
        assert!(registry_excluded(
            Registries {
                url: &base,
                scopes: &none,
                npmrc: &[]
            },
            "ms",
            "^2.0.0",
            30,
            None
        )
        .unwrap()
        .is_empty());
    }

    #[test]
    fn a_cooldown_reads_publish_times_and_skips_younger_releases() {
        let (base, heads) = registry(serde_json::json!({
            "name": "ms",
            "time": {"2.0.0": "2020-01-01T00:00:00.000Z", "2.1.0": "2020-06-01T00:00:00.000Z", "2.1.3": "2026-10-01T00:00:00.000Z"},
            "versions": {
                "2.0.0": {"dist": {"tarball": "https://r/ms-2.0.0.tgz", "shasum": "aa"}},
                "2.1.0": {"dist": {"tarball": "https://r/ms-2.1.0.tgz", "shasum": "bb"}},
                "2.1.3": {"dist": {"tarball": "https://r/ms-2.1.3.tgz", "integrity": "sha512-c"}},
                "2.2.0": {"dist": {"tarball": "https://r/ms-2.2.0.tgz", "integrity": "sha512-d"}}
            }
        }));
        let cutoff = crate::cutoff::timestamp("2026-01-01T00:00:00Z");
        let none = BTreeMap::new();
        let excluded = registry_excluded(
            Registries {
                url: &base,
                scopes: &none,
                npmrc: &[],
            },
            "ms",
            "2.0.0",
            30,
            cutoff,
        )
        .unwrap();
        // 2.1.3 is too young and 2.2.0 has no publish time.
        assert_eq!(excluded[0].version, "2.1.0");
        // Releases from before npm 5 have only a SHA-1 shasum.
        assert_eq!(excluded[0].digest, "sha1:bb");
        let head = heads.recv().unwrap();
        assert!(head.contains("accept: application/json"), "{head}");
    }

    #[test]
    fn scoped_packages_use_their_registry_and_its_credentials() {
        let (base, heads) = registry(serde_json::json!({"versions": {}}));
        let scoped = format!("{base}private/");
        let dir = tempfile::tempdir().unwrap();
        let npmrc = dir.path().join(".npmrc");
        let host = scoped.trim_start_matches("http:");
        fs::write(
            &npmrc,
            format!("{host}:_authToken=\"tok${{DEPSMITH_SURELY_UNSET?}}en\"\n"),
        )
        .unwrap();
        let scopes = BTreeMap::from([("@corp".to_owned(), scoped)]);
        registry_excluded(
            Registries {
                url: NPM_REGISTRY,
                scopes: &scopes,
                npmrc: &[npmrc],
            },
            "@corp/lib",
            "1",
            30,
            None,
        )
        .unwrap();
        let head = heads.recv().unwrap();
        assert!(head.starts_with("get /private/@corp%2flib "), "{head}");
        assert!(head.contains("authorization: bearer token"), "{head}");
    }

    #[test]
    fn npm_configuration_is_layered_quoted_and_expanded() {
        let dir = tempfile::tempdir().unwrap();
        let (project, user) = (dir.path().join("project"), dir.path().join("user"));
        let path = std::env::var("PATH").unwrap_or_default();
        fs::write(
            &project,
            "\u{feff}; comment\n# comment\n@corp:registry = \"https://corp.example/npm\"\n//mirror.example/:_authToken=${PATH}\n",
        )
        .unwrap();
        fs::write(
            &user,
            "registry='https://u:p@mirror.example/'\n@corp:registry=https://ignored.example/\n//mirror.example:_auth=abc\n",
        )
        .unwrap();
        let config = registry_from(&project, Some(user.clone())).unwrap();
        let RegistryConfig::NpmRegistry { url, scopes, npmrc } = config else {
            panic!("{config:?}");
        };
        if std::env::var_os("npm_config_registry").is_none()
            && std::env::var_os("NPM_CONFIG_REGISTRY").is_none()
        {
            assert_eq!(url, "https://mirror.example/");
        }
        assert_eq!(scopes["@corp"], "https://corp.example/npm/");
        // Credentials come from the user's file only: the project's token
        // line, which would send $PATH, is never read.
        assert_eq!(npmrc, [user]);
        assert_eq!(
            authorization(&npmrc, "https://mirror.example/a/b/").unwrap(),
            Some("Basic abc".into())
        );
        assert_eq!(
            authorization(&npmrc, "https://other.example/").unwrap(),
            None
        );
        // The project's references are never expanded, so a registry naming
        // one is not a URL: it fails rather than leaking the variable or
        // silently consulting the public registry.
        fs::write(&project, "registry=https://evil.example/${PATH}\n").unwrap();
        assert!(registry_from(&project, None).is_err());
        assert_eq!(expand_env("a${PATH}b"), format!("a{path}b"));
        assert_eq!(expand_env("${DEPSMITH_SURELY_UNSET?}x"), "x");
        assert_eq!(
            expand_env("${DEPSMITH_SURELY_UNSET}"),
            "${DEPSMITH_SURELY_UNSET}"
        );
        assert_eq!(expand_env(r"\${PATH}"), "${PATH}");
        assert_eq!(expand_env(r"\\${PATH}"), format!(r"\{path}"));
        assert_eq!(expand_env("${a{b}"), "${a{b}");
    }

    #[test]
    fn updates_keeping_git_pins_name_folders_and_fit_the_command_line() {
        let lock = serde_json::json!({"lockfileVersion": 3, "packages": {
            "": {"name": "p"},
            "node_modules/foo": {"name": "ms", "version": "2.0.0", "resolved": "https://r/ms-2.0.0.tgz"},
            "node_modules/a/node_modules/b": {"version": "1.0.0"},
            "node_modules/forked": {"version": "1.0.0", "resolved": "git+ssh://git@x/forked.git#abc"},
            "node_modules/member": {"resolved": "packages/member", "link": true}
        }})
        .to_string();
        let declared = ["foo".to_owned()];
        assert_eq!(
            update_names(&lock, &declared, 0, usize::MAX).unwrap(),
            (vec!["b".to_owned(), "foo".to_owned()], None)
        );
        let (names, note) = update_names(&lock, &declared, 0, 5).unwrap();
        assert_eq!(names, ["foo"]);
        assert!(note.unwrap().contains("only the 1 declared"));
        let error = update_names(&lock, &declared, 0, 3)
            .unwrap_err()
            .to_string();
        assert!(error.contains("--refresh-git"), "{error}");
        assert!(update_names("\u{feff}{}", &declared, 0, 3)
            .unwrap()
            .0
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
    fn the_last_duplicate_key_is_the_declaration() {
        let text = "{\"dependencies\": {\"ms\": \"1\", \"ms\": \"2\"}, \"devDependencies\": {\"ms\": \"3\"}}";
        assert_eq!(&text[value_span(text, "dependencies", "ms").unwrap()], "2");
        let text = "{\"dependencies\": {\"ms\": \"1\"}, \"dependencies\": {\"debug\": \"2\"}}";
        assert!(value_span(text, "dependencies", "ms").is_none());
    }

    #[test]
    fn byte_order_marks_are_accepted_and_omitted_resolutions_kept() {
        assert!(Npm::default().detects(Path::new("package.json"), "\u{feff}{}"));
        let lock = "\u{feff}{\"lockfileVersion\": 3, \"packages\": {\"node_modules/ms\": {\"version\": \"2.0.0\", \"integrity\": \"sha512-a\"}, \"node_modules/bare\": {\"version\": \"1.0.0\"}}}";
        let packages = lock_inventory(lock).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(
            (packages[0].name.as_str(), packages[0].artifact.as_str()),
            ("ms", "sha512-a")
        );
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

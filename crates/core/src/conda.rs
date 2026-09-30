//! The conda adapter: `environment.yml` targets locked with conda-lock. Conda
//! match specs and the `pip:` sub-list are declarations; availability
//! evidence comes from sharded repodata (CEP 16), falling back to full
//! repodata for channels without shards.
use crate::{
    adapter::{
        pypi_key, Adapter, AdapterSpec, Candidate, Capabilities, ManagedFiles, Support, ToolSpec,
    },
    constraints::{AvailabilityConfig, Declaration, Edit, RegistryConfig},
    pixi::Pep508,
    process::run,
    Error, Package, Result, Target, UpdateOptions,
};
use regex::Regex;
use serde::Deserialize;
use serde_yaml::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// Platforms conda-lock locks for when neither the environment nor an
/// existing lock names any.
const DEFAULT_PLATFORMS: [&str; 4] = ["linux-64", "osx-64", "osx-arm64", "win-64"];

/// The conda adapter.
pub struct Conda;

fn parse(text: &str) -> Result<Value> {
    serde_yaml::from_str(text).map_err(|e| Error::Invalid(format!("invalid environment YAML: {e}")))
}

/// The name of a conda match spec and the byte range of its version (and
/// build) part; `None` when unversioned or in bracket form.
fn match_spec(text: &str) -> Option<(String, Option<std::ops::Range<usize>>)> {
    let start = text.len() - text.trim_start().len();
    let body = text[start..]
        .find("::")
        .filter(|i| !text[start..start + i].contains(char::is_whitespace))
        .map_or(start, |i| start + i + 2);
    let name_end = text[body..]
        .find(|c: char| !(c.is_ascii_alphanumeric() || "-_.".contains(c)))
        .map_or(text.len(), |i| body + i);
    if name_end == body {
        return None;
    }
    let rest = &text[name_end..];
    let begin = name_end + (rest.len() - rest.trim_start().len());
    let end = name_end + rest.trim_end().len();
    let range = (begin < end && !text[begin..].starts_with('[')).then_some(begin..end);
    Some((text[body..name_end].to_owned(), range))
}

/// Every string item of a sequence in the document, in order, as
/// (location, text), with locations like `dependencies[4].pip[1]`.
fn string_items(value: &Value, path: &str, output: &mut Vec<(String, String)>) {
    match value {
        Value::Mapping(mapping) => {
            for (key, value) in mapping {
                let key = key.as_str().unwrap_or_default();
                let path = if path.is_empty() {
                    key.to_owned()
                } else {
                    format!("{path}.{key}")
                };
                string_items(value, &path, output);
            }
        }
        Value::Sequence(items) => {
            for (index, item) in items.iter().enumerate() {
                let location = format!("{path}[{index}]");
                match item.as_str() {
                    Some(text) => output.push((location, text.to_owned())),
                    None => string_items(item, &location, output),
                }
            }
        }
        _ => {}
    }
}

/// The value at a location from [`string_items`].
fn slot<'a>(value: &'a mut Value, location: &str) -> Option<&'a mut Value> {
    let mut current = value;
    for segment in location.split('.') {
        let (key, index) = match segment.split_once('[') {
            Some((key, index)) => (
                key,
                Some(index.trim_end_matches(']').parse::<usize>().ok()?),
            ),
            None => (segment, None),
        };
        if !key.is_empty() {
            current = current.get_mut(key)?;
        }
        if let Some(index) = index {
            current = current.get_mut(index)?;
        }
    }
    Some(current)
}

fn declarations_of(parsed: &Value, file: &Path) -> Vec<Declaration> {
    let mut items = vec![];
    if let Some(dependencies) = parsed.get("dependencies") {
        string_items(dependencies, "dependencies", &mut items);
    }
    items
        .into_iter()
        .filter_map(|(location, text)| {
            let pip = location.contains(".pip[");
            let (ecosystem, package, requirement) = if pip {
                let requirement = Pep508::parse(&text)?;
                let version = requirement.specifier.map(|r| text[r].to_owned());
                ("pypi", requirement.name, version)
            } else {
                let (name, range) = match_spec(&text)?;
                ("conda", name, range.map(|r| text[r].to_owned()))
            };
            Some(Declaration {
                ecosystem: ecosystem.into(),
                package,
                requirement: requirement.unwrap_or_default(),
                file: file.into(),
                location,
            })
        })
        .collect()
}

/// The item text after replacing a declaration's requirement.
fn replaced(text: &str, pip: bool, new: &str) -> Option<String> {
    let range = if pip {
        Pep508::parse(text)?.specifier?
    } else {
        match_spec(text)?.1?
    };
    Some(format!(
        "{}{new}{}",
        &text[..range.start],
        &text[range.end..]
    ))
}

/// Apply `edits` to environment `source` by editing only the list items'
/// lines, verified by re-parsing.
fn rewrite_environment(source: &str, edits: &[Edit]) -> Result<String> {
    let mut expected = parse(source)?;
    let mut items = vec![];
    string_items(&expected, "", &mut items);
    let mut plan: BTreeMap<(String, usize), String> = BTreeMap::new();
    for edit in edits {
        let d = &edit.declaration;
        let missing = || {
            Error::Invalid(format!(
                "{} {} is not declared at {} in {}",
                d.package,
                d.requirement,
                d.location,
                d.file.display()
            ))
        };
        let index = items
            .iter()
            .position(|(location, _)| *location == d.location)
            .ok_or_else(missing)?;
        let text = &items[index].1;
        let current = declarations_of(&expected, &d.file)
            .into_iter()
            .find(|c| c.location == d.location);
        if current.as_ref() != Some(d) {
            return Err(missing());
        }
        let new = replaced(text, d.ecosystem == "pypi", &edit.requirement).ok_or_else(missing)?;
        let ordinal = items[..index].iter().filter(|(_, t)| t == text).count();
        plan.insert((text.clone(), ordinal), new.clone());
        *slot(&mut expected, &d.location).ok_or_else(missing)? = Value::String(new);
    }
    let item = Regex::new(r#"^(\s*-\s+)(['"]?)(.*?)(['"]?)(\s*(?:#.*)?)$"#).unwrap();
    let block = Regex::new(r":\s*[|>][+-]?[0-9]?\s*(?:#.*)?$").unwrap();
    let mut block_indent: Option<usize> = None;
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut result = String::new();
    for line in source.split_inclusive('\n') {
        let ending = &line[line.trim_end_matches(['\r', '\n']).len()..];
        let text = line.trim_end_matches(['\r', '\n']);
        let indent = text.len() - text.trim_start().len();
        if let Some(depth) = block_indent {
            if text.trim().is_empty() || indent > depth {
                result.push_str(line);
                continue;
            }
            block_indent = None;
        }
        if block.is_match(text) {
            block_indent = Some(indent);
        }
        if let Some(c) = item.captures(text) {
            if c[2] == c[4] {
                let count = seen.entry(c[3].to_owned()).or_default();
                let ordinal = *count;
                *count += 1;
                if let Some(new) = plan.get(&(c[3].to_owned(), ordinal)) {
                    result.push_str(&format!(
                        "{}{}{new}{}{}{ending}",
                        &c[1], &c[2], &c[4], &c[5]
                    ));
                    continue;
                }
            }
        }
        result.push_str(line);
    }
    if parse(&result).ok() != Some(expected) {
        return Err(Error::Operation(
            "cannot edit this YAML layout without changing unrelated content".into(),
        ));
    }
    Ok(result)
}

/// The conda-lock lock of `environment`: `<stem>.conda-lock.yml` when it
/// exists under `root`, otherwise conda-lock's default `conda-lock.yml`.
fn lock_path(root: &Path, environment: &Path) -> PathBuf {
    let stem = environment
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("environment");
    let named = environment.with_file_name(format!("{stem}.conda-lock.yml"));
    if root.join(&named).is_file() {
        named
    } else {
        environment.with_file_name("conda-lock.yml")
    }
}

/// Resolved packages of a conda-lock unified lock, per platform; `pip`
/// entries belong to the PyPI ecosystem.
///
/// # Errors
///
/// Returns [`Error::Invalid`] when the lock is not valid YAML.
pub fn lock_inventory(text: &str) -> Result<Vec<Package>> {
    let parsed: Value = serde_yaml::from_str(text)
        .map_err(|e| Error::Invalid(format!("invalid conda-lock file: {e}")))?;
    let mut packages = vec![];
    for row in parsed
        .get("package")
        .and_then(Value::as_sequence)
        .into_iter()
        .flatten()
    {
        let field = |name: &str| match row.get(name) {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Number(n)) => Some(n.to_string()),
            _ => None,
        };
        let (Some(name), Some(version), Some(platform)) =
            (field("name"), field("version"), field("platform"))
        else {
            continue;
        };
        packages.push(Package {
            ecosystem: if field("manager").as_deref() == Some("pip") {
                "pypi"
            } else {
                "conda"
            }
            .into(),
            name,
            version,
            artifact: field("url").unwrap_or_default(),
            platform,
        });
    }
    Ok(packages)
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_sequence)
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect()
}

/// Channel URLs for channel names in an environment file.
fn channel_urls(names: &[String]) -> Vec<String> {
    let names: Vec<&str> = if names.is_empty() {
        vec!["defaults"]
    } else {
        names.iter().map(String::as_str).collect()
    };
    names
        .into_iter()
        .filter(|n| *n != "nodefaults")
        .map(|n| {
            if n.starts_with("http://") || n.starts_with("https://") {
                n.trim_end_matches('/').to_owned()
            } else if n == "defaults" {
                "https://repo.anaconda.com/pkgs/main".into()
            } else {
                format!("https://conda.anaconda.org/{n}")
            }
        })
        .collect()
}

/// Bytes stored as msgpack `bin` (or an array of integers).
#[derive(Clone, Default)]
struct Bytes(Vec<u8>);

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Bytes;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("bytes")
            }
            fn visit_bytes<E>(self, v: &[u8]) -> std::result::Result<Bytes, E> {
                Ok(Bytes(v.to_vec()))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Bytes, A::Error> {
                let mut bytes = vec![];
                while let Some(b) = seq.next_element()? {
                    bytes.push(b);
                }
                Ok(Bytes(bytes))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

impl Bytes {
    fn hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[derive(Deserialize, Default)]
struct IndexInfo {
    #[serde(default)]
    base_url: String,
    #[serde(default)]
    shards_base_url: String,
}

#[derive(Deserialize)]
struct ShardIndex {
    #[serde(default)]
    info: IndexInfo,
    shards: BTreeMap<String, Bytes>,
}

#[derive(Deserialize)]
struct Record {
    name: String,
    version: String,
    #[serde(default)]
    timestamp: Option<i64>,
    #[serde(default)]
    sha256: Option<Bytes>,
}

#[derive(Deserialize, Default)]
struct Shard {
    #[serde(default)]
    packages: BTreeMap<String, Record>,
    #[serde(default, rename = "packages.conda")]
    conda: BTreeMap<String, Record>,
}

/// A channel subdir's shard index and the URL it was fetched from.
struct Sharded {
    index: ShardIndex,
    /// The subdir's URL, ending in `/`; relative URLs resolve against it.
    base: String,
}

/// Shard indexes fetched during one preparation, by `channel/subdir`;
/// `None` when the channel serves no shards.
#[derive(Default)]
pub(crate) struct ShardCache(Mutex<BTreeMap<String, Option<Arc<Sharded>>>>);

fn get(url: &str, timeout: u64) -> Result<Option<Vec<u8>>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout))
        .user_agent("depsmith/0.1")
        .build()
        .map_err(|_| Error::Operation("cannot construct HTTP client".into()))?;
    let response = client
        .get(url)
        .send()
        .map_err(|e| Error::Operation(format!("request failed: {}", e.without_url())))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(Error::Operation(format!(
            "{url}: HTTP {}",
            response.status()
        )));
    }
    let bytes = response
        .bytes()
        .map_err(|e| Error::Operation(format!("unreadable response: {}", e.without_url())))?;
    Ok(Some(bytes.to_vec()))
}

fn unzstd(bytes: &[u8], url: &str) -> Result<Vec<u8>> {
    let mut decoder = ruzstd::decoding::StreamingDecoder::new(bytes)
        .map_err(|e| Error::Operation(format!("{url}: invalid zstd data: {e}")))?;
    let mut output = vec![];
    decoder
        .read_to_end(&mut output)
        .map_err(|e| Error::Operation(format!("{url}: invalid zstd data: {e}")))?;
    Ok(output)
}

/// Resolve `relative` against the directory URL `base` (ending in `/`).
fn join(base: &str, relative: &str) -> String {
    if relative.starts_with("http://") || relative.starts_with("https://") {
        relative.to_owned()
    } else {
        format!("{base}{}", relative.trim_start_matches("./"))
    }
}

impl ShardCache {
    fn index(&self, channel: &str, subdir: &str, timeout: u64) -> Result<Option<Arc<Sharded>>> {
        let key = format!("{channel}/{subdir}");
        if let Some(cached) = self.0.lock().unwrap().get(&key) {
            return Ok(cached.clone());
        }
        let base = format!("{key}/");
        let url = format!("{base}repodata_shards.msgpack.zst");
        let index = match get(&url, timeout)? {
            None => None,
            Some(bytes) => {
                let index: ShardIndex = rmp_serde::from_slice(&unzstd(&bytes, &url)?)
                    .map_err(|e| Error::Operation(format!("{url}: unreadable shard index: {e}")))?;
                Some(Arc::new(Sharded { index, base }))
            }
        };
        self.0.lock().unwrap().insert(key, index.clone());
        Ok(index)
    }
}

fn record_json(record: &Record, url: String) -> serde_json::Value {
    let mut json = serde_json::json!({
        "version": record.version,
        "url": url,
        "sha256": record.sha256.as_ref().map_or_else(|| "unknown".into(), Bytes::hex),
    });
    if let Some(timestamp) = record.timestamp {
        json["timestamp"] = timestamp.into();
    }
    json
}

/// Records of `package` per platform across `channels`, in the
/// `{platform: [{version, url, sha256, timestamp}]}` form the conda evidence
/// routine reads. As in a solve, each platform sees its own subdir plus
/// `noarch`.
pub(crate) fn sharded_records(
    cache: &ShardCache,
    channels: &[String],
    platforms: &[String],
    package: &str,
    timeout: u64,
) -> Result<serde_json::Value> {
    let mut subdirs: BTreeSet<&str> = platforms.iter().map(String::as_str).collect();
    subdirs.insert("noarch");
    let mut by_subdir: BTreeMap<&str, Vec<serde_json::Value>> = BTreeMap::new();
    for subdir in subdirs {
        let mut records = vec![];
        for channel in channels {
            match cache.index(channel, subdir, timeout)? {
                Some(index) => {
                    let (index, base) = (&index.index, &index.base);
                    let Some(hash) = index.shards.get(package) else {
                        continue;
                    };
                    let shards = join(base, &index.info.shards_base_url);
                    let url = format!("{shards}{}.msgpack.zst", hash.hex());
                    let bytes = get(&url, timeout)?
                        .ok_or_else(|| Error::Operation(format!("{url}: shard not found")))?;
                    let shard: Shard = rmp_serde::from_slice(&unzstd(&bytes, &url)?)
                        .map_err(|e| Error::Operation(format!("{url}: unreadable shard: {e}")))?;
                    let artifacts = join(base, &index.info.base_url);
                    for (filename, record) in shard.packages.iter().chain(&shard.conda) {
                        if record.name == package {
                            records.push(record_json(record, format!("{artifacts}{filename}")));
                        }
                    }
                }
                None => {
                    let base = format!("{channel}/{subdir}/");
                    let url = format!("{base}repodata.json");
                    let Some(bytes) = get(&url, timeout)? else {
                        continue;
                    };
                    let repodata: serde_json::Value =
                        serde_json::from_slice(&bytes).map_err(|e| {
                            Error::Operation(format!("{url}: unreadable repodata: {e}"))
                        })?;
                    for table in ["packages", "packages.conda"] {
                        for (filename, row) in repodata[table].as_object().into_iter().flatten() {
                            if row["name"] == package {
                                let mut json = row.clone();
                                json["url"] = format!("{base}{filename}").into();
                                records.push(json);
                            }
                        }
                    }
                }
            }
        }
        by_subdir.insert(subdir, records);
    }
    let noarch = by_subdir.get("noarch").cloned().unwrap_or_default();
    let mut output = serde_json::Map::new();
    for platform in platforms {
        let mut records = by_subdir
            .get(platform.as_str())
            .cloned()
            .unwrap_or_default();
        records.extend(noarch.iter().cloned());
        if !records.is_empty() {
            output.insert(platform.clone(), records.into());
        }
    }
    Ok(output.into())
}

/// The platforms the target is locked for: the environment's `platforms:`,
/// else the existing lock's, else conda-lock's defaults.
fn platforms(root: &Path, target: &Target, parsed: &Value) -> Vec<String> {
    let declared = strings(parsed.get("platforms"));
    if !declared.is_empty() {
        return declared;
    }
    let locked = fs::read_to_string(root.join(lock_path(root, &target.manifest)))
        .ok()
        .and_then(|text| serde_yaml::from_str::<Value>(&text).ok())
        .map(|lock| strings(lock.get("metadata").and_then(|m| m.get("platforms"))))
        .unwrap_or_default();
    if locked.is_empty() {
        DEFAULT_PLATFORMS.map(Into::into).to_vec()
    } else {
        locked
    }
}

impl Adapter for Conda {
    fn spec(&self) -> AdapterSpec {
        AdapterSpec {
            manager: "conda".into(),
            ecosystems: vec!["conda".into(), "pypi".into()],
            patterns: vec!["environment.yml".into(), "environment.yaml".into()],
            managed: vec![ManagedFiles {
                manifests: vec!["environment.yml".into(), "environment.yaml".into()],
                inputs: vec!["conda-lock.yml".into(), "environment.conda-lock.yml".into()],
            }],
            skip_dirs: vec![],
            tools: vec![
                ToolSpec {
                    name: "conda-lock".into(),
                    default: "conda-lock".into(),
                    tested_versions: vec!["4.0.2".into()],
                },
                ToolSpec {
                    // The solver conda-lock drives: conda, mamba or micromamba.
                    name: "conda".into(),
                    default: "conda".into(),
                    tested_versions: vec!["2.9.0".into()],
                },
            ],
            capabilities: Capabilities {
                package_selection: Support::Supported,
                constraint_changes: Support::Unsupported(
                    "change a requirement with --accept NAME or --accept NAME=REQUIREMENT".into(),
                ),
                suggestion_acceptance: Support::Supported,
                git_refresh: Support::NotApplicable,
                cooldown: Support::Unsupported(
                    "a release-age cooldown is not implemented for conda-lock".into(),
                ),
                install_validation: Support::NotApplicable,
                lockfile: Support::Supported,
                platforms: Support::Supported,
            },
        }
    }
    fn detects(&self, _: &Path, content: &str) -> bool {
        serde_yaml::from_str::<Value>(content)
            .is_ok_and(|v| v.get("dependencies").is_some_and(Value::is_sequence))
    }
    fn inventory(&self, root: &Path, target: &Target) -> Result<Vec<Package>> {
        let lock = lock_path(root, &target.manifest);
        match fs::read_to_string(root.join(&lock)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::Invalid(format!(
                "{}: no {} to scan; create it with `conda-lock lock` or `depsmith update`",
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
        let declared = self.declarations(root, target)?;
        let mut selected = BTreeMap::new();
        for request in requested {
            let found = declared.iter().find(|d| {
                if d.ecosystem == "pypi" {
                    pypi_key(&d.package) == pypi_key(request)
                } else {
                    d.package.eq_ignore_ascii_case(request.trim())
                }
            });
            if let Some(declaration) = found {
                selected.insert(request.clone(), declaration.package.clone());
            }
        }
        Ok(selected)
    }
    fn declarations(&self, root: &Path, target: &Target) -> Result<Vec<Declaration>> {
        let parsed = parse(&fs::read_to_string(root.join(&target.manifest))?)?;
        Ok(declarations_of(&parsed, &target.manifest))
    }
    fn rewrite(&self, stage: &Path, target: &Target, edits: &[Edit]) -> Result<()> {
        if edits.is_empty() {
            return Ok(());
        }
        if let Some(edit) = edits.iter().find(|e| e.declaration.file != target.manifest) {
            return Err(Error::Invalid(format!(
                "{}: {} is not this target's environment file",
                target.id,
                edit.declaration.file.display()
            )));
        }
        let path = stage.join(&target.manifest);
        let text = rewrite_environment(&fs::read_to_string(&path)?, edits)?;
        fs::write(path, text)?;
        Ok(())
    }
    /// Conda packages come from the environment's channels for its
    /// platforms, PyPI packages from pypi.org.
    fn availability(&self, root: &Path, target: &Target) -> Result<AvailabilityConfig> {
        let parsed = parse(&fs::read_to_string(root.join(&target.manifest))?)?;
        Ok(AvailabilityConfig {
            registries: BTreeMap::from([
                (
                    "conda".into(),
                    RegistryConfig::CondaSharded {
                        channels: channel_urls(&strings(parsed.get("channels"))),
                        platforms: platforms(root, target, &parsed),
                    },
                ),
                (
                    "pypi".into(),
                    RegistryConfig::PypiSimple {
                        indexes: vec![crate::pypi::DEFAULT_INDEX.into()],
                    },
                ),
            ]),
            exclude_newer: None,
        })
    }
    fn prepare(&self, stage: &Path, target: &Target, options: &UpdateOptions) -> Result<Candidate> {
        if options.upgrade {
            return Err(Error::Invalid(
                "--upgrade is not supported by the conda adapter".into(),
            ));
        }
        let environment = stage.join(&target.manifest);
        let source = fs::read(&environment)?;
        let lock = lock_path(stage, &target.manifest);
        let before_text = fs::read_to_string(stage.join(&lock)).ok();
        let before = before_text
            .as_deref()
            .map(lock_inventory)
            .transpose()?
            .unwrap_or_default();
        let solver = options.tool("conda");
        let mut args: Vec<String> = vec!["lock".into(), "--conda".into(), solver.clone()];
        if Path::new(&solver)
            .file_name()
            .is_some_and(|n| n.to_string_lossy().contains("micromamba"))
        {
            args.push("--micromamba".into());
        }
        args.extend([
            "-f".into(),
            environment.to_string_lossy().into(),
            "--lockfile".into(),
            stage.join(&lock).to_string_lossy().into(),
        ]);
        if before_text.is_some() {
            for package in &options.packages {
                args.extend(["--update".into(), package.clone()]);
            }
        }
        let dir = environment.parent().unwrap_or(stage);
        run(
            &options.tool("conda-lock"),
            &args,
            dir,
            options.timeout_seconds,
        )?;
        if fs::read(&environment)? != source {
            return Err(Error::Policy(
                "backend unexpectedly changed the environment file".into(),
            ));
        }
        let after = lock_inventory(&fs::read_to_string(stage.join(&lock))?)?;
        // Every declared package must be locked for every locked platform.
        let locked: BTreeSet<&str> = after.iter().map(|p| p.platform.as_str()).collect();
        for declaration in self.declarations(stage, target)? {
            for platform in &locked {
                let found = after.iter().any(|p| {
                    p.platform == *platform
                        && p.ecosystem == declaration.ecosystem
                        && if p.ecosystem == "pypi" {
                            pypi_key(&p.name) == pypi_key(&declaration.package)
                        } else {
                            p.name.eq_ignore_ascii_case(&declaration.package)
                        }
                });
                if !found {
                    return Err(Error::Operation(format!(
                        "{} is not locked for {platform}; the lock does not match the environment",
                        declaration.package
                    )));
                }
            }
        }
        Ok(Candidate {
            files: vec![target.manifest.clone(), lock],
            suggestions: vec![],
            unresolved: vec![],
            before,
            baseline_available: before_text.is_some(),
            after,
            validation: vec![format!(
                "{}: locked; every declared package is locked on every platform",
                target.id
            )],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_specs_split_name_and_version() {
        let parts = |text: &str| {
            match_spec(text).map(|(name, range)| (name, range.map(|r| text[r].to_owned())))
        };
        for (text, name, version) in [
            ("python=3.12", "python", Some("=3.12")),
            ("conda-forge::six ==1.16.0", "six", Some("==1.16.0")),
            ("numpy >=1.26,<2", "numpy", Some(">=1.26,<2")),
            ("libblas 3.9.* *mkl", "libblas", Some("3.9.* *mkl")),
            ("pip", "pip", None),
            ("pkg[version='>=1']", "pkg", None),
        ] {
            assert_eq!(
                parts(text),
                Some((name.into(), version.map(Into::into))),
                "{text}"
            );
        }
    }

    #[test]
    fn identical_items_are_edited_by_location() {
        let source = "dependencies:\n  - six ==1.16.0\n  - pip:\n    - six==1.16.0\n  - 'six ==1.16.0'  # again\n";
        let declared = declarations_of(&parse(source).unwrap(), Path::new("environment.yml"));
        let edit = Edit {
            declaration: declared[2].clone(),
            requirement: "==1.17.0".into(),
            comment: None,
        };
        assert_eq!(declared[2].location, "dependencies[2]");
        assert_eq!(
            rewrite_environment(source, &[edit]).unwrap(),
            source.replace("'six ==1.16.0'", "'six ==1.17.0'")
        );
    }

    #[test]
    fn shard_indexes_and_shards_decode() {
        #[derive(serde::Serialize)]
        struct Index<'a> {
            info: BTreeMap<&'a str, &'a str>,
            shards: BTreeMap<&'a str, serde_bytes_like::Bytes<'a>>,
        }
        mod serde_bytes_like {
            pub struct Bytes<'a>(pub &'a [u8]);
            impl serde::Serialize for Bytes<'_> {
                fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                    s.serialize_bytes(self.0)
                }
            }
        }
        let index = Index {
            info: BTreeMap::from([("base_url", ""), ("shards_base_url", "shards/")]),
            shards: BTreeMap::from([("six", serde_bytes_like::Bytes(&[0xab, 0x01]))]),
        };
        let packed = rmp_serde::to_vec_named(&index).unwrap();
        let compressed = ruzstd::encoding::compress_to_vec(
            &packed[..],
            ruzstd::encoding::CompressionLevel::Fastest,
        );
        let decoded: ShardIndex =
            rmp_serde::from_slice(&unzstd(&compressed, "test").unwrap()).unwrap();
        assert_eq!(decoded.shards["six"].hex(), "ab01");
        assert_eq!(
            join("https://c/noarch/", &decoded.info.shards_base_url),
            "https://c/noarch/shards/"
        );
    }
}

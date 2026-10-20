//! Resolving JavaScript and TypeScript specifiers: to first-party source
//! files by relative path, tsconfig `paths` and `baseUrl`, or npm workspace
//! package, else to the package they name.
use super::{js::Dialect, usage::slashed};
use crate::Target;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
};

/// The build output directories a workspace package's entry points can name
/// before it is built, whose sources are in `src/`.
const BUILD_DIRS: &[&str] = &["dist", "lib", "build", "out"];

/// How deep a chain of tsconfig `extends` is followed.
const EXTENDS_DEPTH: usize = 16;

/// The extensions a specifier without one is tried with, in order.
const EXTENSIONS: &[&str] = &["ts", "tsx", "d.ts", "js", "jsx", "mts", "cts", "mjs", "cjs"];

/// What a specifier resolves to.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Resolved {
    /// First-party source files, by index.
    Files(Vec<usize>),
    /// The package it names, such as `@scope/pkg` for `@scope/pkg/sub`.
    Package(String),
    /// Nothing to follow: a `node:` built-in, a URL, or a first-party file
    /// that is not a source file or is missing.
    Nothing,
}

/// Resolves the specifiers of a target's JavaScript and TypeScript files.
pub(super) struct Resolver {
    /// Each scanned source file, relative to the root, by its index.
    files: BTreeMap<PathBuf, usize>,
    /// The tsconfig options that govern each directory of a source file.
    configs: BTreeMap<PathBuf, Config>,
    /// The target's npm workspace packages, the target's own included.
    workspace: Vec<WorkspacePackage>,
}

/// A package of the target's npm workspace.
#[derive(Debug)]
struct WorkspacePackage {
    /// Its `name`.
    name: String,
    /// Its directory, relative to the root.
    dir: PathBuf,
    /// Its `package.json`.
    manifest: Value,
}

/// The module resolution options of a tsconfig, with its `extends` applied.
#[derive(Clone, Debug, Default)]
struct Config {
    /// `baseUrl`, relative to the root.
    base_url: Option<PathBuf>,
    /// `paths`: each pattern with its substitutions, in order.
    paths: Vec<(String, Vec<String>)>,
    /// The directory of the tsconfig that sets `paths`.
    paths_dir: PathBuf,
}

impl Resolver {
    /// A resolver over the JavaScript and TypeScript files among `sources`,
    /// paths relative to `root`, of `target`. A tsconfig or workspace
    /// manifest that cannot be read adds a warning to `warnings`.
    pub(super) fn new(
        root: &Path,
        target: &Target,
        sources: &[PathBuf],
        warnings: &mut Vec<String>,
    ) -> Self {
        let dir = target.manifest.parent().unwrap_or(Path::new(""));
        let files: BTreeMap<PathBuf, usize> = sources
            .iter()
            .enumerate()
            .filter(|(_, source)| Dialect::of(source).is_some())
            .map(|(index, source)| (source.clone(), index))
            .collect();
        let mut loaded: BTreeMap<PathBuf, Config> = BTreeMap::new();
        let mut configs = BTreeMap::new();
        for source in files.keys() {
            let parent = source.parent().unwrap_or(Path::new(""));
            if configs.contains_key(parent) {
                continue;
            }
            // The nearest tsconfig.json, else jsconfig.json, at or above the
            // file in the target's directory.
            let nearest = parent
                .ancestors()
                .take_while(|ancestor| ancestor.starts_with(dir))
                .find_map(|ancestor| {
                    ["tsconfig.json", "jsconfig.json"]
                        .iter()
                        .map(|name| ancestor.join(name))
                        .find(|config| root.join(config).is_file())
                });
            if let Some(path) = nearest {
                let config = loaded
                    .entry(path.clone())
                    .or_insert_with(|| load(root, &path, 0, warnings))
                    .clone();
                configs.insert(parent.to_owned(), config);
            }
        }
        Self {
            files,
            configs,
            workspace: workspace(root, target, warnings),
        }
    }

    /// What `specifier`, imported by `importer`, resolves to. A workspace
    /// package whose entry cannot be found resolves to all its files, with a
    /// warning added to `warnings`.
    pub(super) fn resolve(
        &self,
        importer: &Path,
        specifier: &str,
        warnings: &mut Vec<String>,
    ) -> Resolved {
        let from = importer.parent().unwrap_or(Path::new(""));
        if is_relative(specifier) {
            return self.files(normalize(&from.join(specifier)).as_deref());
        }
        if let Some(config) = self.configs.get(from) {
            let index = self.alias(config, specifier).or_else(|| {
                let path = normalize(&config.base_url.as_ref()?.join(specifier))?;
                self.probe(&path)
            });
            if let Some(index) = index {
                return Resolved::Files(vec![index]);
            }
        }
        // `node:` built-ins, URLs, virtual modules, and package `#imports`.
        if specifier.contains(':') || specifier.starts_with('#') || specifier.starts_with('/') {
            return Resolved::Nothing;
        }
        for package in &self.workspace {
            let subpath = match specifier.strip_prefix(package.name.as_str()) {
                Some("") => ".".to_owned(),
                Some(rest) if rest.starts_with('/') => format!(".{rest}"),
                _ => continue,
            };
            if let Some(index) = self.entry(package, &subpath) {
                return Resolved::Files(vec![index]);
            }
            let warning = format!(
                "cannot find what {specifier} names in workspace package {}, so all its files count",
                package.name
            );
            if !warnings.contains(&warning) {
                warnings.push(warning);
            }
            // Its files, without those of the packages nested in it.
            let nested: Vec<&Path> = self
                .workspace
                .iter()
                .filter(|other| other.dir != package.dir && other.dir.starts_with(&package.dir))
                .map(|other| other.dir.as_path())
                .collect();
            return Resolved::Files(
                self.files
                    .iter()
                    .filter(|(file, _)| {
                        file.starts_with(&package.dir)
                            && !nested.iter().any(|dir| file.starts_with(dir))
                    })
                    .map(|(_, &index)| index)
                    .collect(),
            );
        }
        // A built-in's bare name, such as `buffer`, is still a package name:
        // bundlers replace it with the npm package of that name.
        package_name(specifier).map_or(Resolved::Nothing, |name| Resolved::Package(name.to_owned()))
    }

    /// The source file `specifier` names by the `paths` of `config`: the
    /// first substitution of its exact pattern, else of its longest matching
    /// wildcard pattern, that names one. As in TypeScript, `None` lets
    /// resolution go on.
    fn alias(&self, config: &Config, specifier: &str) -> Option<usize> {
        let mut best: Option<(usize, &str, &[String])> = None;
        for (pattern, substitutions) in &config.paths {
            match pattern.split_once('*') {
                None if pattern == specifier => {
                    best = Some((usize::MAX, "", substitutions));
                    break;
                }
                None => {}
                Some((prefix, suffix)) => {
                    let matched = specifier.len() >= prefix.len() + suffix.len()
                        && specifier.starts_with(prefix)
                        && specifier.ends_with(suffix);
                    if matched && best.is_none_or(|(length, ..)| prefix.len() > length) {
                        let star = &specifier[prefix.len()..specifier.len() - suffix.len()];
                        best = Some((prefix.len(), star, substitutions));
                    }
                }
            }
        }
        let (_, star, substitutions) = best?;
        let base = config.base_url.as_ref().unwrap_or(&config.paths_dir);
        substitutions.iter().find_map(|substitution| {
            let path = normalize(&base.join(substitution.replace('*', star)))?;
            self.probe(&path)
        })
    }

    /// The source file `subpath` (`.` or `./x`) of workspace `package`
    /// names: by its `exports`, else for `.` its `module`, `main` or
    /// `index`, else the path in its directory. An entry in build output
    /// that is not there names its source in `src/`.
    fn entry(&self, package: &WorkspacePackage, subpath: &str) -> Option<usize> {
        let manifest = &package.manifest;
        let declared = exported(manifest.get("exports"), subpath).or_else(|| {
            (subpath == ".")
                .then(|| {
                    ["module", "main"]
                        .iter()
                        .find_map(|field| manifest.get(field)?.as_str())
                })
                .flatten()
        });
        let Some(declared) = declared else {
            return if subpath == "." {
                self.index(&package.dir)
            } else {
                self.probe(&normalize(&package.dir.join(subpath))?)
            };
        };
        let path = normalize(&package.dir.join(declared))?;
        self.probe(&path).or_else(|| {
            let relative = path.strip_prefix(&package.dir).ok()?;
            let mut parts = relative.components();
            let first = parts.next()?.as_os_str().to_str()?;
            if !BUILD_DIRS.contains(&first) {
                return None;
            }
            let mut source = package.dir.join("src").join(parts.as_path());
            // `index.js` and `index.d.ts` both name the source `index`.
            let name = source.file_name()?.to_str()?;
            let stem = EXTENSIONS
                .iter()
                .find_map(|extension| name.strip_suffix(&format!(".{extension}")))
                .unwrap_or(name)
                .to_owned();
            source.set_file_name(stem);
            self.probe(&source)
        })
    }

    fn files(&self, path: Option<&Path>) -> Resolved {
        match path.and_then(|path| self.probe(path)) {
            Some(index) => Resolved::Files(vec![index]),
            None => Resolved::Nothing,
        }
    }

    /// The source file `path` names: itself, the TypeScript source of a
    /// JavaScript extension, `path` with an extension, or its directory's
    /// `index`.
    fn probe(&self, path: &Path) -> Option<usize> {
        if let Some(&index) = self.files.get(path) {
            return Some(index);
        }
        let twins: &[&str] = match path.extension().and_then(|e| e.to_str()) {
            Some("js") => &["ts", "tsx"],
            Some("jsx") => &["tsx"],
            Some("mjs") => &["mts"],
            Some("cjs") => &["cts"],
            _ => &[],
        };
        twins
            .iter()
            .find_map(|twin| self.files.get(&path.with_extension(twin)).copied())
            .or_else(|| self.with_extension(path))
            .or_else(|| self.index(path))
    }

    /// The source file `path` names with one of the extensions added.
    fn with_extension(&self, path: &Path) -> Option<usize> {
        EXTENSIONS.iter().find_map(|extension| {
            let mut name = path.as_os_str().to_owned();
            name.push(".");
            name.push(extension);
            self.files.get(Path::new(&name)).copied()
        })
    }

    /// The `index` source file of directory `dir`.
    fn index(&self, dir: &Path) -> Option<usize> {
        self.with_extension(&dir.join("index"))
    }
}

/// The packages of `target`'s npm workspace, its own included, that have a
/// name; none when its manifest is not a `package.json`.
fn workspace(root: &Path, target: &Target, warnings: &mut Vec<String>) -> Vec<WorkspacePackage> {
    if target
        .manifest
        .file_name()
        .is_none_or(|name| name != "package.json")
    {
        return vec![];
    }
    let manifests = match crate::npm::manifests(root, target) {
        Ok(manifests) => manifests,
        Err(e) => {
            warnings.push(e.to_string());
            return vec![];
        }
    };
    manifests
        .into_iter()
        .filter_map(|path| match crate::npm::read(root, &path) {
            Ok(manifest) => Some(WorkspacePackage {
                name: manifest.get("name")?.as_str()?.to_owned(),
                dir: path.parent().unwrap_or(Path::new("")).to_owned(),
                manifest,
            }),
            Err(e) => {
                warnings.push(e.to_string());
                None
            }
        })
        .collect()
}

/// The file `exports` maps `subpath` to: its literal key, or for `.` the
/// whole value when it holds no subpaths.
fn exported<'a>(exports: Option<&'a Value>, subpath: &str) -> Option<&'a str> {
    let exports = exports?;
    let has_subpaths = exports
        .as_object()
        .is_some_and(|map| map.keys().any(|key| key.starts_with('.')));
    if has_subpaths {
        condition(exports.get(subpath)?)
    } else if subpath == "." {
        condition(exports)
    } else {
        None
    }
}

/// The first path under the `types`, `import`, `require` or `default`
/// condition of an `exports` value.
fn condition(value: &Value) -> Option<&str> {
    match value {
        Value::String(path) => Some(path),
        Value::Array(choices) => choices.iter().find_map(condition),
        Value::Object(conditions) => ["types", "import", "require", "default"]
            .iter()
            .find_map(|name| condition(conditions.get(*name)?)),
        _ => None,
    }
}

/// The options of the tsconfig at `path`, relative to `root`, over those of
/// the relative tsconfigs it `extends`, in order. A package it extends, such
/// as `@tsconfig/node20`, is not followed.
fn load(root: &Path, path: &Path, depth: usize, warnings: &mut Vec<String>) -> Config {
    let parsed = std::fs::read_to_string(root.join(path))
        .map_err(|e| e.to_string())
        .and_then(|text| {
            serde_json::from_str::<Value>(&jsonc(text.strip_prefix('\u{feff}').unwrap_or(&text)))
                .map_err(|e| e.to_string())
        });
    let parsed = match parsed {
        Ok(parsed) => parsed,
        Err(e) => {
            warnings.push(format!("cannot read {}: {e}", slashed(path)));
            return Config::default();
        }
    };
    let dir = path.parent().unwrap_or(Path::new(""));
    let extends = match parsed.get("extends") {
        Some(Value::String(one)) => vec![one.as_str()],
        Some(Value::Array(many)) => many.iter().filter_map(Value::as_str).collect(),
        _ => vec![],
    };
    let mut config = Config::default();
    for parent in extends.into_iter().filter(|e| is_relative(e)) {
        let Some(mut parent) = normalize(&dir.join(parent)) else {
            continue;
        };
        if !root.join(&parent).is_file() && parent.extension().is_none_or(|e| e != "json") {
            parent.as_mut_os_string().push(".json");
        }
        if depth < EXTENDS_DEPTH {
            let inherited = load(root, &parent, depth + 1, warnings);
            if inherited.base_url.is_some() {
                config.base_url = inherited.base_url;
            }
            if !inherited.paths.is_empty() {
                config.paths = inherited.paths;
                config.paths_dir = inherited.paths_dir;
            }
        }
    }
    let options = parsed.get("compilerOptions");
    if let Some(base) = options
        .and_then(|o| o.get("baseUrl"))
        .and_then(Value::as_str)
    {
        config.base_url = normalize(&dir.join(base));
    }
    if let Some(paths) = options
        .and_then(|o| o.get("paths"))
        .and_then(Value::as_object)
    {
        config.paths = paths
            .iter()
            .map(|(pattern, substitutions)| {
                let substitutions = substitutions
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect();
                (pattern.clone(), substitutions)
            })
            .collect();
        config.paths_dir = dir.to_owned();
    }
    config
}

/// `text`, JSON with comments and trailing commas as tsconfig allows, as
/// plain JSON.
fn jsonc(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                output.push(c);
                while let Some(c) = chars.next() {
                    output.push(c);
                    match c {
                        '\\' => output.extend(chars.next()),
                        '"' => break,
                        _ => {}
                    }
                }
            }
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        output.push(c);
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut last = ' ';
                for c in chars.by_ref() {
                    if last == '*' && c == '/' {
                        break;
                    }
                    last = c;
                }
                output.push(' ');
            }
            _ => output.push(c),
        }
    }
    // Then drop each comma that only whitespace separates from a closing
    // bracket, outside strings.
    let mut cleaned = String::with_capacity(output.len());
    let mut in_string = false;
    let mut escaped = false;
    for (at, c) in output.char_indices() {
        if in_string {
            in_string = !(c == '"' && !escaped);
            escaped = c == '\\' && !escaped;
        } else if c == '"' {
            in_string = true;
        } else if c == ',' && output[at + 1..].trim_start().starts_with(['}', ']']) {
            continue;
        }
        cleaned.push(c);
    }
    cleaned
}

fn is_relative(specifier: &str) -> bool {
    matches!(specifier, "." | "..") || specifier.starts_with("./") || specifier.starts_with("../")
}

/// The package a bare specifier names: its first segment, or its first two
/// for a scoped package.
fn package_name(specifier: &str) -> Option<&str> {
    let end = if specifier.starts_with('@') {
        let scope = specifier.find('/')?;
        specifier[scope + 1..]
            .find('/')
            .map_or(specifier.len(), |at| scope + 1 + at)
    } else {
        specifier.find('/').unwrap_or(specifier.len())
    };
    Some(&specifier[..end]).filter(|name| !name.is_empty())
}

/// `path` with `.` and `..` resolved, or `None` when it leaves the root.
fn normalize(path: &Path) -> Option<PathBuf> {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => output.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                if !output.pop() {
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(output)
}

/// The package `name` types when it is `@types/x`: `x`, or `@scope/pkg` for
/// `@types/scope__pkg`.
pub(super) fn typed_package(name: &str) -> Option<String> {
    let typed = name.strip_prefix("@types/")?;
    Some(match typed.split_once("__") {
        Some((scope, name)) => format!("@{scope}/{name}"),
        None => typed.to_owned(),
    })
}

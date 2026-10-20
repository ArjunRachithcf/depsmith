//! Import names: the modules each Python or conda package provides.
use super::tables::PYTHON_IMPORT_NAMES;
use crate::{adapter::pypi_key as normalize, Target};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

/// The modules each Python or conda package provides, read from a target's
/// installed environments; packages they do not hold fall back to
/// [`PYTHON_IMPORT_NAMES`](super::PYTHON_IMPORT_NAMES), then to their
/// normalized name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportNames {
    /// Normalized package name to the modules it provides.
    installed: BTreeMap<String, BTreeSet<String>>,
    warnings: Vec<String>,
}

impl ImportNames {
    /// The import names installed for `target` under `root`, the worktree:
    /// its uv environment (the `.venv` in its directory, else in the nearest
    /// directory above it), and every Pixi environment in its directory, with
    /// both their conda packages and their PyPI packages. Without any of
    /// them, every name falls back. [`ImportNames::warnings`] says so, and
    /// names each environment file that cannot be read.
    pub fn read(root: &Path, target: &Target) -> Self {
        let dir = target.manifest.parent().unwrap_or(Path::new(""));
        let mut names = Self::default();
        let mut environments = vec![];
        if let Some(venv) = std::iter::successors(Some(dir), |d| d.parent())
            .map(|d| root.join(d).join(".venv"))
            .find(|venv| venv.is_dir())
        {
            environments.push(venv);
        }
        let pixi = root.join(dir).join(".pixi").join("envs");
        for env in names.entries(&pixi) {
            if env.is_dir() {
                names.read_conda_meta(&env.join("conda-meta"));
                environments.push(env);
            }
        }
        for env in &environments {
            for site in names.site_packages(env) {
                names.read_site_packages(&site);
            }
        }
        if environments.is_empty() {
            let shown = if dir.as_os_str().is_empty() {
                ".".into()
            } else {
                dir.display().to_string()
            };
            names.warnings.push(format!(
                "no installed environment in {shown}, so import names come from the built-in table"
            ));
        }
        names
    }

    /// Record that `package` provides `modules`, as an installed environment
    /// would.
    pub fn insert(&mut self, package: &str, modules: impl IntoIterator<Item = String>) {
        self.installed
            .entry(normalize(package))
            .or_default()
            .extend(modules);
    }

    /// The modules `package` provides, sorted: as installed, else as the
    /// built-in table has them, else its name in lowercase with `-` and `.`
    /// as `_`. A stub package (`types-X`, `X-stubs`) provides the modules of
    /// the package it types.
    pub fn modules(&self, package: &str) -> Vec<String> {
        let mut name = normalize(package);
        if let Some(typed) = name
            .strip_prefix("types-")
            .or_else(|| name.strip_suffix("-stubs"))
        {
            name = typed.to_owned();
        }
        if let Some(modules) = self.installed.get(&name).filter(|m| !m.is_empty()) {
            return modules.iter().cloned().collect();
        }
        if let Some((_, modules)) = PYTHON_IMPORT_NAMES.iter().find(|(n, _)| *n == name) {
            let mut modules: Vec<String> = modules.iter().map(|m| (*m).to_owned()).collect();
            modules.sort();
            return modules;
        }
        vec![name.replace('-', "_")]
    }

    /// What could not be read, such as a target without an environment.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// The paths in `dir`, sorted, or none with a warning when it cannot be
    /// listed.
    fn entries(&mut self, dir: &Path) -> Vec<PathBuf> {
        if !dir.is_dir() {
            return vec![];
        }
        let listed = fs::read_dir(dir).and_then(|entries| {
            entries
                .map(|entry| entry.map(|e| e.path()))
                .collect::<std::io::Result<Vec<_>>>()
        });
        match listed {
            Ok(mut paths) => {
                paths.sort();
                paths
            }
            Err(e) => {
                self.warn(dir, &e);
                vec![]
            }
        }
    }

    fn warn(&mut self, path: &Path, error: &dyn std::fmt::Display) {
        self.warnings
            .push(format!("cannot read {}: {error}", path.display()));
    }

    /// The `site-packages` directories of the environment `env`: `lib/*/` on
    /// Unix, `Lib/` on Windows.
    fn site_packages(&mut self, env: &Path) -> Vec<PathBuf> {
        let mut sites: Vec<PathBuf> = self
            .entries(&env.join("lib"))
            .into_iter()
            .chain([env.join("Lib")])
            .map(|dir| dir.join("site-packages"))
            .filter(|site| site.is_dir())
            .collect();
        sites.dedup();
        sites
    }

    /// Each `*.dist-info` in `site`, by its `RECORD`, else its
    /// `top_level.txt`, and each `*.egg-info` by its `top_level.txt`.
    fn read_site_packages(&mut self, site: &Path) {
        for path in self.entries(site) {
            let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            // `name-version.dist-info`, whose name has its `-` escaped as `_`
            // (though not always in older installs), and
            // `name-version[-pyX.Y].egg-info`.
            let package = if let Some(stem) = file_name.strip_suffix(".dist-info") {
                stem.rsplit_once('-').map_or(stem, |(name, _)| name)
            } else if let Some(stem) = file_name.strip_suffix(".egg-info") {
                stem.split('-').next().unwrap_or(stem)
            } else {
                continue;
            };
            let record = path.join("RECORD");
            let modules = if record.is_file() {
                match fs::read_to_string(&record) {
                    Ok(record) => modules_of(record.lines().filter_map(record_path)),
                    Err(e) => {
                        self.warn(&record, &e);
                        continue;
                    }
                }
            } else if let Ok(top) = fs::read_to_string(path.join("top_level.txt")) {
                top.lines()
                    .map(str::trim)
                    .filter(|l| is_identifier(l))
                    .map(str::to_owned)
                    .collect()
            } else {
                continue;
            };
            self.insert(package, modules);
        }
    }

    /// Each conda package in `conda_meta`, by the files it installs in
    /// `site-packages`.
    fn read_conda_meta(&mut self, conda_meta: &Path) {
        for path in self.entries(conda_meta) {
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let record = fs::read(&path).map_err(|e| e.to_string()).and_then(|text| {
                serde_json::from_slice::<serde_json::Value>(&text).map_err(|e| e.to_string())
            });
            let record = match record {
                Ok(record) => record,
                Err(e) => {
                    self.warn(&path, &e);
                    continue;
                }
            };
            let Some(name) = record.get("name").and_then(|n| n.as_str()) else {
                continue;
            };
            // `lib/pythonX.Y/site-packages/…`, `Lib/site-packages/…`, or
            // `site-packages/…` for a noarch package.
            let files = record
                .get("files")
                .and_then(|f| f.as_array())
                .into_iter()
                .flatten()
                .filter_map(|f| f.as_str())
                .filter_map(|f| {
                    f.strip_prefix("site-packages/")
                        .or_else(|| f.split_once("/site-packages/").map(|(_, rest)| rest))
                });
            self.insert(name, modules_of(files));
        }
    }
}

/// The path of a `RECORD` line, its first CSV field.
fn record_path(line: &str) -> Option<&str> {
    match line.strip_prefix('"') {
        Some(quoted) => quoted.split_once('"').map(|(path, _)| path),
        None => line.split(',').next(),
    }
    .filter(|p| !p.is_empty())
}

/// Top-level directories of an installed package that are never importable
/// packages, although they can hold `.py` files.
const NOT_PACKAGES: &[&str] = &["tests", "test", "docs", "doc", "examples", "benchmarks"];

/// The modules that `files`, paths relative to `site-packages`, provide: each
/// top-level module or regular package, and below a namespace package (a
/// directory without `__init__.py`) each module or regular package it holds.
/// Editable-install finders and `.pth` files provide nothing.
fn modules_of<'a>(files: impl Iterator<Item = &'a str>) -> BTreeSet<String> {
    let files: Vec<Vec<&str>> = files
        .filter(|f| !f.starts_with("../") && !f.starts_with('/'))
        .map(|f| f.split('/').collect::<Vec<_>>())
        .filter(|parts| {
            !parts
                .iter()
                .any(|p| *p == "__pycache__" || p.ends_with(".dist-info") || p.ends_with(".data"))
        })
        .collect();
    let packages: BTreeSet<String> = files
        .iter()
        .filter(|parts| parts.len() > 1 && parts.last() == Some(&"__init__.py"))
        .map(|parts| parts[..parts.len() - 1].join("/"))
        .collect();
    let mut modules = BTreeSet::new();
    'files: for parts in &files {
        if parts.len() > 1 && NOT_PACKAGES.contains(&parts[0]) {
            continue;
        }
        // Descend through namespace packages to the first regular package,
        // or to a module file.
        let mut module = vec![];
        for (depth, part) in parts.iter().enumerate() {
            if depth + 1 == parts.len() {
                match module_file(part) {
                    Some(stem) if stem != "__init__" && !stem.starts_with("__editable__") => {
                        module.push(stem)
                    }
                    _ => continue 'files,
                }
                break;
            }
            if !is_identifier(part) {
                continue 'files;
            }
            module.push(*part);
            if packages.contains(&parts[..=depth].join("/")) {
                break;
            }
        }
        modules.insert(module.join("."));
    }
    modules
}

/// The module a file at the top of a package provides: `six` for `six.py`,
/// `_yaml` for `_yaml.cpython-312-x86_64-linux-gnu.so`.
fn module_file(file: &str) -> Option<&str> {
    let (stem, _) = file.split_once('.')?;
    let extension = file.rsplit('.').next()?;
    matches!(extension, "py" | "so" | "pyd").then_some(stem)
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c == '_' || c.is_alphabetic())
        && chars.all(|c| c == '_' || c.is_alphanumeric())
}

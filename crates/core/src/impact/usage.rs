//! Usage sites of affected packages in a target's first-party code, and the
//! first-party import graph walked in reverse from them.
use super::{
    change::Affected,
    imports::{Import, ImportKind, Scan},
    names::ImportNames,
    python::python_imports,
};
use crate::{Error, Result, Target};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::{Path, PathBuf},
};

/// Directories that never hold first-party code: environments, installed
/// packages and build output.
const SKIPPED_DIRS: &[&str] = &[
    ".venv",
    "venv",
    ".pixi",
    "node_modules",
    "site-packages",
    "__pycache__",
    "dist",
    "build",
];

/// A first-party import of an affected package.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageSite {
    /// The importing file, relative to the root.
    pub file: PathBuf,
    /// The 1-based line of the import.
    pub line: usize,
    /// The module imported, such as `idna.core`.
    pub module: String,
    /// The affected package it belongs to.
    pub package: String,
}

/// One step of a chain: the import on `line` of `file` that leads on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hop {
    /// The file, relative to the root.
    pub file: PathBuf,
    /// The 1-based line of the import.
    pub line: usize,
}

/// A first-party file that imports an affected package, directly or through
/// other first-party files.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AffectedFile {
    /// The file, relative to the root.
    pub file: PathBuf,
    /// The affected packages it reaches, by name.
    pub packages: Vec<String>,
    /// A shortest chain of imports from this file to a usage site, whose last
    /// hop is the site itself.
    pub chain: Vec<Hop>,
}

/// Where a target's first-party code uses the affected packages.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usages {
    /// Each usage site, by file and line.
    pub sites: Vec<UsageSite>,
    /// Each affected file, by path.
    pub files: Vec<AffectedFile>,
    /// What the scan could not read, with the warnings of the import names
    /// first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Where the first-party Python code of `target` under `root` uses the
/// packages in `affected`, and which first-party files reach those uses.
///
/// Every `.py` file in the target's directory is scanned, honouring ignore
/// files and skipping hidden directories, environments (`.venv`, `venv`,
/// `.pixi`), installed packages and build output (`dist`, `build`). For a
/// side read from git, call [`Sides::sources`](super::Sides::sources) first.
/// A module resolves to first-party code under the deepest root it is in:
/// the target's directory, any directory below it with a `pyproject.toml`
/// (such as a workspace member), or the `src/` directory of either.
/// Importing a module also loads the `__init__.py` of each package above it,
/// and a directory of modules without one is a namespace package. An import
/// that resolves nowhere there is matched against the modules the affected
/// packages provide, as `names` has them, and the standard-library modules
/// built on them: it matches a module it names or one below it, and for
/// `from a import b` also `a.b`. The warnings of `names` come first.
///
/// # Errors
///
/// Returns [`crate::Error::Operation`] when the target's directory cannot be
/// walked.
pub fn usages(
    root: &Path,
    target: &Target,
    affected: &Affected,
    names: &ImportNames,
) -> Result<Usages> {
    let dir = target.manifest.parent().unwrap_or(Path::new(""));
    let (sources, projects) = python_files(root, dir)?;
    let mut warnings = names.warnings().to_vec();
    let scans: Vec<Scan> = sources
        .iter()
        .map(|file| match std::fs::read_to_string(root.join(file)) {
            Ok(text) => {
                let scan = python_imports(&text);
                for line in &scan.unparsed {
                    warnings.push(format!(
                        "{}:{line}: the parser could not read this, so its imports are skipped",
                        slashed(file)
                    ));
                }
                scan
            }
            Err(e) => {
                warnings.push(format!("{}: cannot read: {e}", slashed(file)));
                Scan::default()
            }
        })
        .collect();

    let mut roots = vec![dir.to_owned()];
    roots.extend(projects);
    for project in roots.clone() {
        if root.join(&project).join("src").is_dir() {
            roots.push(project.join("src"));
        }
    }
    let modules = Modules::new(&sources, &roots);
    let provided = provided_modules(affected, names);

    let mut sites = vec![];
    let mut importers: Vec<Vec<Edge>> = vec![vec![]; sources.len()];
    for (index, scan) in scans.iter().enumerate() {
        for import in &scan.imports {
            let resolved = modules.resolve(index, import);
            if !resolved.is_empty() {
                for imported in resolved {
                    importers[imported].push(Edge {
                        importer: index,
                        line: import.line,
                    });
                }
                continue;
            }
            let top = import.module.split('.').next().unwrap_or_default();
            if import.level > 0 || modules.is_first_party(top) {
                continue;
            }
            let mut candidates = vec![import.module.clone()];
            if import.kind == ImportKind::From {
                candidates.extend(
                    import
                        .names
                        .iter()
                        .map(|n| format!("{}.{n}", import.module)),
                );
            }
            for (prefix, package) in &provided {
                let matched = candidates.iter().find(|candidate| {
                    candidate
                        .strip_prefix(prefix.as_str())
                        .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
                });
                if let Some(module) = matched {
                    sites.push(Site {
                        file: index,
                        line: import.line,
                        module: module.clone(),
                        package: package.clone(),
                    });
                }
            }
        }
    }
    sites.sort();
    sites.dedup();

    let files = affected_files(&sources, &importers, &sites);
    Ok(Usages {
        sites: sites
            .into_iter()
            .map(|site| UsageSite {
                file: sources[site.file].clone(),
                line: site.line,
                module: site.module,
                package: site.package,
            })
            .collect(),
        files,
        warnings,
    })
}

/// A first-party import: file `importer` imports another on `line`.
#[derive(Clone, Copy, Debug)]
struct Edge {
    importer: usize,
    line: usize,
}

/// A usage site, with its file by index.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Site {
    file: usize,
    line: usize,
    module: String,
    package: String,
}

/// The affected files: every file a usage site is in or that imports one,
/// with the packages it reaches and a shortest chain to a site.
fn affected_files(
    sources: &[PathBuf],
    importers: &[Vec<Edge>],
    sites: &[Site],
) -> Vec<AffectedFile> {
    // A breadth-first walk from every site at once finds a shortest chain:
    // `onward[file]` is the edge that leads on towards a site, or `None` at a
    // site.
    let mut onward: BTreeMap<usize, Option<(usize, usize)>> = BTreeMap::new();
    let mut site_lines: BTreeMap<usize, usize> = BTreeMap::new();
    let mut queue = VecDeque::new();
    for site in sites {
        if onward.insert(site.file, None).is_none() {
            site_lines.insert(site.file, site.line);
            queue.push_back(site.file);
        }
    }
    while let Some(file) = queue.pop_front() {
        for edge in &importers[file] {
            if let std::collections::btree_map::Entry::Vacant(entry) = onward.entry(edge.importer) {
                entry.insert(Some((file, edge.line)));
                queue.push_back(edge.importer);
            }
        }
    }
    // Then one walk per package finds the packages each file reaches.
    let mut packages: BTreeMap<usize, BTreeSet<&str>> = BTreeMap::new();
    let mut starts: BTreeMap<&str, BTreeSet<usize>> = BTreeMap::new();
    for site in sites {
        starts.entry(&site.package).or_default().insert(site.file);
    }
    for (package, mut seen) in starts {
        let mut queue: VecDeque<usize> = seen.iter().copied().collect();
        while let Some(file) = queue.pop_front() {
            packages.entry(file).or_default().insert(package);
            for edge in &importers[file] {
                if seen.insert(edge.importer) {
                    queue.push_back(edge.importer);
                }
            }
        }
    }
    let mut files: Vec<AffectedFile> = onward
        .keys()
        .map(|&start| {
            let mut chain = vec![];
            let mut file = start;
            loop {
                let (next, line) = match onward[&file] {
                    Some((next, line)) => (Some(next), line),
                    None => (None, site_lines[&file]),
                };
                chain.push(Hop {
                    file: sources[file].clone(),
                    line,
                });
                match next {
                    Some(next) => file = next,
                    None => break,
                }
            }
            AffectedFile {
                file: sources[start].clone(),
                packages: packages[&start].iter().map(|p| (*p).to_owned()).collect(),
                chain,
            }
        })
        .collect();
    files.sort_by(|a, b| a.file.cmp(&b.file));
    files
}

/// The `.py` files under `dir`, sorted, and the directories among them that
/// hold a `pyproject.toml`, all relative to `root`.
fn python_files(root: &Path, dir: &Path) -> Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    let walker = ignore::WalkBuilder::new(root.join(dir))
        .require_git(false)
        .filter_entry(|e| {
            !(e.file_type().is_some_and(|t| t.is_dir())
                && e.file_name()
                    .to_str()
                    .is_some_and(|n| SKIPPED_DIRS.contains(&n)))
        })
        .build();
    let mut sources = vec![];
    let mut projects = vec![];
    for entry in walker {
        let entry = entry.map_err(|e| Error::Operation(e.to_string()))?;
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(root) else {
            continue;
        };
        if relative.extension().is_some_and(|e| e == "py") {
            sources.push(relative.to_owned());
        } else if entry.depth() > 1 && entry.file_name() == "pyproject.toml" {
            projects.push(relative.parent().unwrap_or(dir).to_owned());
        }
    }
    sources.sort();
    projects.sort();
    Ok((sources, projects))
}

/// The first-party modules: the dotted name of each source file under the
/// deepest root it is in.
struct Modules {
    /// Module name to the files that define it.
    files: BTreeMap<String, Vec<usize>>,
    /// Every package name, including namespace packages without an
    /// `__init__.py`.
    packages: BTreeSet<String>,
    /// Each file's module name, when it is under a root.
    names: Vec<Option<ModuleName>>,
}

/// The dotted name of a source file's module.
#[derive(Clone, Debug)]
struct ModuleName {
    name: String,
    /// Whether the file is a package's `__init__.py`.
    package: bool,
}

impl Modules {
    fn new(sources: &[PathBuf], roots: &[PathBuf]) -> Self {
        let mut files: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        let mut packages = BTreeSet::new();
        let mut names = vec![None; sources.len()];
        for (index, source) in sources.iter().enumerate() {
            let name = roots
                .iter()
                .filter_map(|root| Some((root, source.strip_prefix(root).ok()?)))
                .max_by_key(|(root, _)| root.components().count())
                .and_then(|(_, relative)| module_name(relative));
            if let Some(module) = &name {
                files.entry(module.name.clone()).or_default().push(index);
                packages.extend(parents(&module.name).map(str::to_owned));
                if module.package {
                    packages.insert(module.name.clone());
                }
            }
            names[index] = name;
        }
        Self {
            files,
            packages,
            names,
        }
    }

    fn is_first_party(&self, top: &str) -> bool {
        self.files.contains_key(top) || self.packages.contains(top)
    }

    /// The first-party files `import`, in file `index`, loads: the module it
    /// names, and the `__init__.py` of each package above it.
    fn resolve(&self, index: usize, import: &Import) -> Vec<usize> {
        let base = if import.level == 0 {
            import.module.clone()
        } else {
            let Some(module) = &self.names[index] else {
                return vec![];
            };
            let mut parts: Vec<&str> = module.name.split('.').collect();
            if !module.package {
                parts.pop();
            }
            for _ in 1..import.level {
                if parts.pop().is_none() {
                    return vec![];
                }
            }
            parts.extend(import.module.split('.').filter(|p| !p.is_empty()));
            parts.join(".")
        };
        let mut found: Vec<&str> = vec![];
        if import.kind == ImportKind::From {
            for name in &import.names {
                let submodule = if base.is_empty() {
                    name.clone()
                } else {
                    format!("{base}.{name}")
                };
                if let Some((key, _)) = self.files.get_key_value(&submodule) {
                    found.push(key);
                }
            }
        }
        if found.is_empty() {
            let mut candidate = base.as_str();
            while !candidate.is_empty() {
                if let Some((key, _)) = self.files.get_key_value(candidate) {
                    found.push(key);
                    break;
                }
                candidate = candidate.rsplit_once('.').map_or("", |(parent, _)| parent);
            }
        }
        let mut loaded: BTreeSet<usize> = BTreeSet::new();
        for module in found {
            for name in std::iter::once(module).chain(parents(module)) {
                loaded.extend(self.files.get(name).into_iter().flatten());
            }
        }
        loaded.into_iter().collect()
    }
}

/// The packages above `module`, innermost first: `a.b` and `a` for `a.b.c`.
fn parents(module: &str) -> impl Iterator<Item = &str> {
    module.rmatch_indices('.').map(|(at, _)| &module[..at])
}

/// The dotted module name of `relative`, a `.py` path below a root.
fn module_name(relative: &Path) -> Option<ModuleName> {
    let mut parts: Vec<&str> = relative
        .parent()?
        .components()
        .map(|c| c.as_os_str().to_str())
        .collect::<Option<_>>()?;
    let stem = relative.file_stem()?.to_str()?;
    let package = stem == "__init__";
    if !package {
        parts.push(stem);
    }
    (!parts.is_empty()).then(|| ModuleName {
        name: parts.join("."),
        package,
    })
}

/// The modules the affected Python and conda packages provide, each with
/// its package: what `names` says each provides, and the standard-library
/// modules built on it.
fn provided_modules(affected: &Affected, names: &ImportNames) -> BTreeSet<(String, String)> {
    let mut provided = BTreeSet::new();
    for package in &affected.packages {
        if !matches!(package.ecosystem.as_str(), "pypi" | "conda") {
            continue;
        }
        for module in names
            .modules(&package.name)
            .into_iter()
            .chain(package.stdlib.iter().cloned())
        {
            provided.insert((module, package.name.clone()));
        }
    }
    provided
}

/// `path` with `/` between its components on every platform.
fn slashed(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

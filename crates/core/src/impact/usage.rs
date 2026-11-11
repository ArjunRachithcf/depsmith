//! Usage sites of affected packages in a target's first-party code, and the
//! first-party import graph walked in reverse from them.
use super::{
    change::Affected,
    imports::{Import, ImportKind, Literal, Scan},
    js::{js_scan, Dialect},
    names::ImportNames,
    python::python_scan,
    references::{
        first_party, npm_bins, package_scripts, pixi_tasks, ByNameKind, ByNameReference, Matcher,
    },
    specifiers::{typed_package, Resolved, Resolver},
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
    ".next",
    "coverage",
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
    /// Each package named without an import, by file and line.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub by_name: Vec<ByNameReference>,
    /// What the scan could not read, with the warnings of the import names
    /// first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Where the first-party code of `target` under `root` uses the packages in
/// `affected`, and which first-party files reach those uses.
///
/// The `.py` files in the target's directory are scanned when a Python or
/// conda package is affected, and its JavaScript and TypeScript files when
/// an npm package is, honouring ignore files and skipping hidden
/// directories, environments (`.venv`, `venv`, `.pixi`), installed packages
/// and build and coverage output (`dist`, `build`, `coverage`). For a side
/// read from git, call [`Sides::sources`](super::Sides::sources) first.
///
/// A Python module resolves to first-party code under the deepest root it
/// is in: the target's directory, any directory below it with a
/// `pyproject.toml` (such as a workspace member) unless that directory is
/// itself a package, or the `src/` directory of either. Importing a module
/// also loads the `__init__.py` of each package above it, and a directory of
/// modules without one is a namespace package.
/// An import that resolves nowhere there is matched against the modules the
/// affected packages provide, as `names` has them, and the standard-library
/// modules built on them: it matches a module it names or one below it, and
/// for `from a import b` also `a.b`. The warnings of `names` come first.
///
/// A JavaScript or TypeScript specifier resolves to a first-party file by
/// relative path (`.js` naming its `.ts` source, a directory its `index`),
/// by the `paths` and `baseUrl` of the nearest `tsconfig.json` (else
/// `jsconfig.json`), through its
/// relative `extends`, or by the name of an npm workspace package, whose
/// unbuilt entry in `dist/` names its source in `src/`. Any other specifier
/// names a package, matched against the affected npm packages, with
/// `@types/x` matching `x`; `node:` built-ins never match.
///
/// A package named without an import is a [`ByNameReference`]: a source string
/// whose whole value is its name or a module it provides, or a command
/// that runs it (a list, or a string a call such as `subprocess.run`
/// launches, naming its executable or `-m` module), which also seeds the
/// reverse walk; and a Pixi task or `package.json` script that runs it. An
/// executable names the packages the installed environment, `node_modules`
/// or [`EXECUTABLES`](super::EXECUTABLES) say install it, else its own name.
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
    let has = |ecosystems: &[&str]| {
        affected
            .packages
            .iter()
            .any(|p| ecosystems.contains(&p.ecosystem.as_str()))
    };
    let (python, javascript) = (has(&["pypi", "conda"]), has(&["npm"]));
    let walked = walk(root, dir)?;
    let mut warnings = names.warnings().to_vec();
    if javascript && walked.unscanned > 0 {
        let dir = slashed(dir);
        warnings.push(format!(
            "{} .vue, .svelte or .astro files in {} are not scanned",
            walked.unscanned,
            if dir.is_empty() { "." } else { &dir }
        ));
    }
    // Each source with its dialect, `None` for Python.
    let (sources, dialects): (Vec<PathBuf>, Vec<Option<Dialect>>) = walked
        .sources
        .into_iter()
        .map(|source| {
            let dialect = Dialect::of(&source);
            (source, dialect)
        })
        .filter(|(_, dialect)| {
            if dialect.is_some() {
                javascript
            } else {
                python
            }
        })
        .unzip();
    let (scans, literals): (Vec<_>, Vec<Vec<Literal>>) = sources
        .iter()
        .zip(&dialects)
        .map(
            |(file, dialect)| match std::fs::read_to_string(root.join(file)) {
                Ok(text) => {
                    let found = match *dialect {
                        Some(dialect) => js_scan(&text, dialect),
                        None => python_scan(&text),
                    };
                    for line in &found.scan.unparsed {
                        warnings.push(format!(
                            "{}:{line}: the parser could not read this, so its imports are skipped",
                            slashed(file)
                        ));
                    }
                    (found.scan, found.literals)
                }
                Err(e) => {
                    warnings.push(format!("{}: cannot read: {e}", slashed(file)));
                    (Scan::default(), vec![])
                }
            },
        )
        .unzip();

    let mut roots = vec![];
    let projects = walked.projects;
    for project in std::iter::once(dir.to_owned()).chain(projects.iter().cloned()) {
        if root.join(&project).join("src").is_dir() {
            roots.push(project.join("src"));
        }
        // A project whose directory is itself a package, holding an
        // `__init__.py`, is imported by its directory's name from the
        // directory above it.
        if project != dir && root.join(&project).join("__init__.py").is_file() {
            roots.push(project.parent().unwrap_or(dir).to_owned());
        } else {
            roots.push(project);
        }
    }
    roots.sort();
    roots.dedup();
    let modules = Modules::new(&sources, &roots);
    let provided = provided_modules(affected, names);
    let resolver = Resolver::new(root, target, &sources, &mut warnings);
    let npm = npm_packages(affected);

    let mut sites = vec![];
    let mut importers: Vec<Vec<Edge>> = vec![vec![]; sources.len()];
    for (index, scan) in scans.iter().enumerate() {
        for import in &scan.imports {
            let edge = Edge {
                importer: index,
                line: import.line,
            };
            if dialects[index].is_some() {
                match resolver.resolve(&sources[index], &import.module, &mut warnings) {
                    Resolved::Files(files) => {
                        for imported in files {
                            importers[imported].push(edge);
                        }
                    }
                    Resolved::Package(name) => {
                        for package in npm.get(name.as_str()).into_iter().flatten() {
                            sites.push(Site {
                                file: index,
                                line: import.line,
                                module: import.module.clone(),
                                package: (*package).to_owned(),
                            });
                        }
                    }
                    Resolved::Nothing => {}
                }
                continue;
            }
            let resolved = modules.resolve(index, import);
            if !resolved.is_empty() {
                for imported in resolved {
                    importers[imported].push(edge);
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

    // Packages named without an import: in source strings, which also
    // seed the reverse walk, and in Pixi tasks and package scripts.
    let mut tasks = vec![(target.manifest.clone(), pixi_tasks(root, target))];
    if crate::npm::is_package(target) {
        for manifest in crate::npm::manifests(root, target).unwrap_or_default() {
            let scripts = package_scripts(root, &manifest);
            tasks.push((manifest, scripts));
        }
    }
    let manifest_dirs: Vec<PathBuf> = tasks
        .iter()
        .map(|(manifest, _)| manifest.parent().unwrap_or(Path::new("")).to_owned())
        .collect();
    let bins = npm_bins(root, &manifest_dirs);
    let first_party = first_party(root, target, &projects);
    let matcher = Matcher::new(
        affected
            .packages
            .iter()
            .map(|p| (p.ecosystem.as_str(), p.name.as_str())),
        &first_party,
        &provided,
        Box::new(|executable: &str| {
            let mut packages = names.executable(executable);
            packages.extend(bins.get(executable).into_iter().flatten().cloned());
            packages
        }),
    );
    let mut references = vec![];
    let mut seeds = sites.clone();
    for (index, file_literals) in literals.iter().enumerate() {
        for literal in file_literals {
            for (package, name) in matcher.literal(literal) {
                let imported = sites
                    .iter()
                    .any(|s| s.file == index && s.line == literal.line && s.package == package);
                if imported {
                    continue;
                }
                seeds.push(Site {
                    file: index,
                    line: literal.line,
                    module: name.clone(),
                    package: package.to_owned(),
                });
                references.push(ByNameReference {
                    file: sources[index].clone(),
                    line: literal.line,
                    package: package.to_owned(),
                    name,
                    kind: ByNameKind::Source,
                });
            }
        }
    }
    for (file, tasks) in tasks {
        for (line, command) in &tasks.commands {
            for (package, name) in matcher.command(command, &tasks.names) {
                references.push(ByNameReference {
                    line: *line,
                    file: file.clone(),
                    package: package.to_owned(),
                    name,
                    kind: ByNameKind::Task,
                });
            }
        }
    }
    references.sort();
    references.dedup();
    seeds.sort();
    seeds.dedup();

    let files = affected_files(&sources, &importers, &seeds);
    Ok(Usages {
        by_name: references,
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

/// The source files of a target's directory.
struct Walked {
    /// The Python, JavaScript and TypeScript files, sorted, relative to the
    /// root.
    sources: Vec<PathBuf>,
    /// The directories below the target's that hold a `pyproject.toml`.
    projects: Vec<PathBuf>,
    /// How many `.vue`, `.svelte` and `.astro` files there are, which hold
    /// imports the scanners do not read.
    unscanned: usize,
}

/// The source files under `dir`, relative to `root`.
fn walk(root: &Path, dir: &Path) -> Result<Walked> {
    let walker = ignore::WalkBuilder::new(root.join(dir))
        .require_git(false)
        .filter_entry(|e| {
            !(e.file_type().is_some_and(|t| t.is_dir())
                && e.file_name()
                    .to_str()
                    .is_some_and(|n| SKIPPED_DIRS.contains(&n)))
        })
        .build();
    let mut walked = Walked {
        sources: vec![],
        projects: vec![],
        unscanned: 0,
    };
    for entry in walker {
        let entry = entry.map_err(|e| Error::Operation(e.to_string()))?;
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(root) else {
            continue;
        };
        let extension = relative.extension().and_then(|e| e.to_str());
        if extension == Some("py") || Dialect::of(relative).is_some() {
            walked.sources.push(relative.to_owned());
        } else if matches!(extension, Some("vue" | "svelte" | "astro")) {
            walked.unscanned += 1;
        } else if entry.depth() > 1 && entry.file_name() == "pyproject.toml" {
            walked
                .projects
                .push(relative.parent().unwrap_or(dir).to_owned());
        }
    }
    walked.sources.sort();
    walked.projects.sort();
    Ok(walked)
}

/// The first-party Python modules: the dotted name of each `.py` source
/// file under the deepest root it is in. Other source files, such as
/// JavaScript or TypeScript, are ignored.
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
            if source.extension().is_none_or(|e| e != "py") {
                continue;
            }
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

/// The affected npm packages by the package name imports use: each by its
/// own, and `@types/x` also by `x`.
fn npm_packages(affected: &Affected) -> BTreeMap<String, Vec<&str>> {
    let mut packages: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for package in affected.packages.iter().filter(|p| p.ecosystem == "npm") {
        let name = package.name.as_str();
        packages.entry(name.to_owned()).or_default().push(name);
        if let Some(typed) = typed_package(name) {
            packages.entry(typed).or_default().push(name);
        }
    }
    packages
}

/// `path` with `/` between its components on every platform.
pub(super) fn slashed(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

//! By-name references: packages a target names without importing them, in
//! source strings, commands, tasks and scripts.
use super::{imports::Literal, tables::EXECUTABLES};
use crate::{adapter::pypi_key, Target};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

/// Where a package is named.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ByNameKind {
    /// A string in source code, such as `"-m", "nuitka"`.
    Source,
    /// A build requirement.
    Build,
    /// A Pixi task or a `package.json` script.
    Task,
    /// A configuration that loads it, such as a Babel or test runner config.
    Config,
    /// A lint or hook configuration.
    Tooling,
}

/// A package a first-party file names without importing it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ByNameReference {
    /// The file, relative to the root.
    pub file: PathBuf,
    /// The 1-based line it is named on.
    pub line: usize,
    /// The affected package it names.
    pub package: String,
    /// How it is named, such as `tsc` for `typescript`.
    pub name: String,
    /// Where it is named.
    pub kind: ByNameKind,
}

/// What a command names: the executable it runs, or the module
/// `python -m` runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Named {
    Executable(String),
    Module(String),
}

/// The words of a command line; `None` for one that is not a literal, such
/// as `sys.executable`.
pub(super) type Words = Vec<Option<String>>;

/// A command that runs another: by its subcommands (none: it runs its
/// first argument), and the flags it takes a value after.
struct Wrapper {
    name: &'static str,
    subcommands: &'static [&'static str],
    value_flags: &'static [&'static str],
}

const WRAPPERS: &[Wrapper] = &[
    Wrapper {
        name: "bun",
        subcommands: &["run", "x"],
        value_flags: &[],
    },
    Wrapper {
        name: "bunx",
        subcommands: &[],
        value_flags: &["-p", "--package"],
    },
    Wrapper {
        name: "cross-env",
        subcommands: &[],
        value_flags: &[],
    },
    Wrapper {
        name: "env",
        subcommands: &[],
        value_flags: &["-u", "--unset", "-C", "--chdir"],
    },
    Wrapper {
        name: "npm",
        subcommands: &["exec", "x"],
        value_flags: &["-p", "--package", "-w", "--workspace"],
    },
    Wrapper {
        name: "npx",
        subcommands: &[],
        value_flags: &["-p", "--package"],
    },
    Wrapper {
        name: "pixi",
        subcommands: &["exec", "run"],
        value_flags: &[
            "-e",
            "--environment",
            "--manifest-path",
            "-s",
            "--spec",
            "--with",
        ],
    },
    Wrapper {
        name: "pnpm",
        subcommands: &["dlx", "exec"],
        value_flags: &["-C", "--dir", "-F", "--filter"],
    },
    Wrapper {
        name: "pnpx",
        subcommands: &[],
        value_flags: &[],
    },
    Wrapper {
        name: "poetry",
        subcommands: &["run"],
        value_flags: &["-C", "--directory"],
    },
    Wrapper {
        name: "time",
        subcommands: &[],
        value_flags: &[],
    },
    Wrapper {
        name: "uv",
        subcommands: &["run"],
        value_flags: &[
            "--with",
            "--from",
            "-p",
            "--python",
            "--project",
            "--directory",
            "--package",
            "--group",
            "--extra",
            "-w",
        ],
    },
    Wrapper {
        name: "uvx",
        subcommands: &[],
        value_flags: &["--with", "--from", "-p", "--python"],
    },
    Wrapper {
        name: "yarn",
        subcommands: &["dlx", "exec"],
        value_flags: &["-p", "--package"],
    },
];

/// Package managers whose bare `pm x` runs script `x` when it has one,
/// else the executable `x`, unless `x` is one of the manager's own
/// commands; `run x` always names a script or task.
const RUNNERS: &[&str] = &["bun", "npm", "pixi", "pnpm", "yarn"];

/// The package managers' own commands, which run no executable.
const MANAGER_COMMANDS: &[&str] = &[
    "add",
    "audit",
    "cache",
    "ci",
    "config",
    "create",
    "i",
    "info",
    "init",
    "install",
    "link",
    "list",
    "ls",
    "outdated",
    "publish",
    "remove",
    "test",
    "up",
    "upgrade",
    "why",
    "workspace",
    "workspaces",
];

/// What a command line names: in each of its commands (split at `&&`,
/// `||`, `;`, `|`, `&` and newlines), after variable assignments and
/// wrappers such as `npx` or `uv run`, the executable it runs, or the module
/// after `-m`. A script or task named in `scripts`, which a runner such as
/// `npm run` runs, names nothing.
pub(super) fn command_names(tokens: &[Option<String>], scripts: &BTreeSet<String>) -> Vec<Named> {
    let mut named = vec![];
    for command in tokens.split(|token| {
        token
            .as_deref()
            .is_some_and(|t| matches!(t, "&&" | "||" | ";" | "|" | "&"))
    }) {
        let mut rest = command.iter().peekable();
        'command: while let Some(token) = rest.next() {
            let Some(token) = token.as_deref() else {
                // An interpreter given by value, such as `sys.executable`,
                // runs `-m module` like `python` does.
                if let Some(module) = module_after(&mut rest) {
                    named.push(Named::Module(module));
                }
                break;
            };
            if is_assignment(token) {
                continue;
            }
            let program = program(token);
            if RUNNERS.contains(&program.as_str()) {
                if let Some(next) = rest.peek().and_then(|t| t.as_deref()) {
                    let script = if next == "run" {
                        rest.next();
                        rest.peek().and_then(|t| t.as_deref())
                    } else {
                        Some(next)
                    };
                    if script.is_some_and(|s| scripts.contains(s) || MANAGER_COMMANDS.contains(&s))
                    {
                        break;
                    }
                }
            }
            if let Some(wrapper) = WRAPPERS.iter().find(|w| w.name == program) {
                if !wrapper.subcommands.is_empty() {
                    let next = rest.peek().and_then(|t| t.as_deref());
                    if next.is_some_and(|n| wrapper.subcommands.contains(&n)) {
                        rest.next();
                    } else if !RUNNERS.contains(&program.as_str()) {
                        break;
                    }
                }
                // Its flags, and the values of those that take one.
                while let Some(flag) = rest.peek().and_then(|t| t.as_deref()) {
                    if !flag.starts_with('-') {
                        break;
                    }
                    rest.next();
                    if wrapper.value_flags.contains(&flag) {
                        rest.next();
                    }
                    if flag == "--" {
                        break;
                    }
                }
                continue 'command;
            }
            if is_python(&program) {
                if let Some(module) = module_after(&mut rest) {
                    named.push(Named::Module(module));
                }
                break;
            }
            named.push(Named::Executable(program));
            break;
        }
    }
    named
}

/// The program `token` runs: its file name, without a Windows extension.
fn program(token: &str) -> String {
    let name = token.rsplit(['/', '\\']).next().unwrap_or(token);
    [".exe", ".cmd", ".bat", ".ps1"]
        .iter()
        .find_map(|extension| name.strip_suffix(extension))
        .unwrap_or(name)
        .to_owned()
}

fn is_assignment(token: &str) -> bool {
    token.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

fn is_python(program: &str) -> bool {
    program == "py"
        || program
            .strip_prefix("python")
            .is_some_and(|version| version.chars().all(|c| c.is_ascii_digit() || c == '.'))
}

/// The module of a `-m module` among the next tokens.
fn module_after<'a>(tokens: &mut impl Iterator<Item = &'a Option<String>>) -> Option<String> {
    while let Some(token) = tokens.next() {
        match token.as_deref() {
            Some("-m") => return tokens.next()?.clone(),
            // The flags that take a value.
            Some("-X" | "-W") => {
                tokens.next();
            }
            Some(flag) if flag.starts_with('-') && flag != "-c" => {}
            _ => return None,
        }
    }
    None
}

/// The words of `line`, split at whitespace with shell quoting and
/// backslash escapes. `&&`, `||`, `;`, `|` and `&` are words of their own,
/// and so are newlines and parentheses, as `;`.
pub(super) fn words(line: &str) -> Words {
    let mut words = vec![];
    let mut word = String::new();
    let mut quoted = false;
    let mut quote = None;
    let mut chars = line.chars().peekable();
    let push = |word: &mut String, quoted: &mut bool, words: &mut Words| {
        if !word.is_empty() || *quoted {
            words.push(Some(std::mem::take(word)));
        }
        *quoted = false;
    };
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some('"'), '\\') => word.extend(chars.next()),
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => word.push(c),
            (None, '\\') => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                } else {
                    word.extend(chars.next());
                }
            }
            (None, '"' | '\'') => {
                quote = Some(c);
                quoted = true;
            }
            (None, '\n' | ';' | '(' | ')') => {
                push(&mut word, &mut quoted, &mut words);
                words.push(Some(";".into()));
            }
            (None, c) if c.is_whitespace() => push(&mut word, &mut quoted, &mut words),
            (None, '&' | '|') => {
                push(&mut word, &mut quoted, &mut words);
                let mut operator = c.to_string();
                if chars.peek() == Some(&c) {
                    operator.push(c);
                    chars.next();
                }
                words.push(Some(operator));
            }
            (None, c) => word.push(c),
        }
    }
    push(&mut word, &mut quoted, &mut words);
    words
}

/// Matches names against the affected packages of one target.
pub(super) struct Matcher<'a> {
    /// Affected package by its key: normalized for Python and conda, as
    /// written for npm.
    packages: BTreeMap<String, Vec<&'a str>>,
    /// The modules the affected packages provide, each with its package.
    provided: &'a BTreeSet<(String, String)>,
    /// What installs each executable: the installed environment, else
    /// nothing.
    installed: Installed<'a>,
}

/// The packages the installed environment says install an executable.
pub(super) type Installed<'a> = Box<dyn Fn(&str) -> Vec<String> + 'a>;

impl<'a> Matcher<'a> {
    /// A matcher of `packages`, by ecosystem and name, except those named
    /// in `first_party`, the target's own projects.
    pub(super) fn new(
        packages: impl IntoIterator<Item = (&'a str, &'a str)>,
        first_party: &BTreeSet<String>,
        provided: &'a BTreeSet<(String, String)>,
        installed: Installed<'a>,
    ) -> Self {
        let mut keyed: BTreeMap<String, Vec<&str>> = BTreeMap::new();
        for (ecosystem, name) in packages {
            if first_party.contains(name) || first_party.contains(&pypi_key(name)) {
                continue;
            }
            keyed.entry(key(ecosystem, name)).or_default().push(name);
        }
        Self {
            packages: keyed,
            provided,
            installed,
        }
    }

    /// The affected packages called `name`: as written, or normalized
    /// unless `name` is in capitals, like a constant or a config key.
    fn package(&self, name: &str) -> Vec<&'a str> {
        let mut found: Vec<&str> = vec![];
        let capitals =
            name.chars().any(char::is_alphabetic) && !name.chars().any(char::is_lowercase);
        let normalized = (!capitals).then(|| pypi_key(name));
        for key in std::iter::once(name.to_owned()).chain(normalized) {
            extend_new(
                &mut found,
                self.packages.get(&key).into_iter().flatten().copied(),
            );
        }
        found
    }

    /// The affected packages that provide module `module`, or one above it.
    fn module(&self, module: &str) -> Vec<&'a str> {
        let mut found: Vec<&str> = vec![];
        for (prefix, package) in self.provided {
            let matched = module
                .strip_prefix(prefix.as_str())
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'));
            if matched {
                extend_new(&mut found, self.package(package));
            }
        }
        found
    }

    /// The affected packages that install executable `name`.
    fn executable(&self, name: &str) -> Vec<&'a str> {
        let mut candidates: Vec<String> = (self.installed)(name);
        if let Some((_, packages)) = EXECUTABLES.iter().find(|(e, _)| *e == name) {
            candidates.extend(packages.iter().map(|p| (*p).to_owned()));
        }
        candidates.push(name.to_owned());
        let mut found: Vec<&str> = vec![];
        for candidate in candidates {
            extend_new(&mut found, self.package(&candidate));
        }
        found
    }

    /// The packages a command line names, each with the name it uses; the
    /// tasks or scripts in `scripts` name none.
    pub(super) fn command(
        &self,
        tokens: &[Option<String>],
        scripts: &BTreeSet<String>,
    ) -> Vec<(&'a str, String)> {
        let mut found = vec![];
        for named in command_names(tokens, scripts) {
            let (packages, name) = match named {
                Named::Executable(name) => (self.executable(&name), name),
                Named::Module(module) => (self.module(&module), module),
            };
            found.extend(packages.into_iter().map(|p| (p, name.clone())));
        }
        found
    }

    /// The packages a source literal names: a string whose whole value is
    /// a package or a module it provides, or a command.
    pub(super) fn literal(&self, literal: &Literal) -> Vec<(&'a str, String)> {
        let mut found = vec![];
        if let [Some(value)] = literal.values.as_slice() {
            for package in self
                .package(value)
                .into_iter()
                .chain(self.module_exactly(value))
            {
                found.push((package, value.clone()));
            }
            if literal.command {
                found.extend(self.command(&words(value), &BTreeSet::new()));
            }
        } else if literal.values.len() > 1 {
            found.extend(self.command(&literal.values, &BTreeSet::new()));
        }
        found
    }

    /// The affected packages that provide exactly module `module`.
    fn module_exactly(&self, module: &str) -> Vec<&'a str> {
        self.provided
            .iter()
            .filter(|(provided, _)| provided == module)
            .flat_map(|(_, package)| self.package(package))
            .collect()
    }
}

/// Adds to `found` each of `packages` it does not hold yet.
fn extend_new<'a>(found: &mut Vec<&'a str>, packages: impl IntoIterator<Item = &'a str>) {
    for package in packages {
        if !found.contains(&package) {
            found.push(package);
        }
    }
}

/// The key a package is matched by: normalized for Python and conda, as
/// written for npm.
fn key(ecosystem: &str, name: &str) -> String {
    if ecosystem == "npm" {
        name.to_owned()
    } else {
        pypi_key(name)
    }
}

/// The executables each package in the `node_modules` of each of `dirs`
/// installs, by the `bin` of its `package.json`.
pub(super) fn npm_bins(root: &Path, dirs: &[PathBuf]) -> BTreeMap<String, Vec<String>> {
    let mut bins: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut manifests = vec![];
    let entries = dirs
        .iter()
        .flat_map(|dir| {
            fs::read_dir(root.join(dir).join("node_modules"))
                .into_iter()
                .flatten()
        })
        .flatten();
    for entry in entries {
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with('@') {
            for scoped in fs::read_dir(&path).into_iter().flatten().flatten() {
                manifests.push(scoped.path().join("package.json"));
            }
        } else {
            manifests.push(path.join("package.json"));
        }
    }
    for manifest in manifests {
        let Ok(text) = fs::read_to_string(&manifest) else {
            continue;
        };
        let Ok(parsed) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let Some(name) = parsed.get("name").and_then(Value::as_str) else {
            continue;
        };
        let executables: Vec<String> = match parsed.get("bin") {
            Some(Value::String(_)) => vec![name.rsplit('/').next().unwrap_or(name).to_owned()],
            Some(Value::Object(map)) => map.keys().cloned().collect(),
            _ => vec![],
        };
        for executable in executables {
            let packages = bins.entry(executable).or_default();
            if !packages.iter().any(|p| p == name) {
                packages.push(name.to_owned());
            }
        }
    }
    bins
}

/// The names of the target's own projects, each also normalized: the
/// `[project]`, Poetry or Pixi (`[workspace]`, `[package]`) name of its
/// manifest and of each `pyproject.toml` in `projects`, and the npm
/// workspace's packages.
pub(super) fn first_party(root: &Path, target: &Target, projects: &[PathBuf]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let manifests = std::iter::once(target.manifest.clone())
        .chain(projects.iter().map(|p| p.join("pyproject.toml")));
    for manifest in manifests {
        let Ok(text) = fs::read_to_string(root.join(&manifest)) else {
            continue;
        };
        if let Ok(parsed) = text.parse::<toml::Table>() {
            let tables = [
                parsed.get("project"),
                parsed.get("workspace"),
                parsed.get("package"),
                parsed.get("tool").and_then(|t| t.get("poetry")),
                parsed
                    .get("tool")
                    .and_then(|t| t.get("pixi"))
                    .and_then(|t| t.get("package")),
            ];
            for name in tables
                .into_iter()
                .flatten()
                .filter_map(|t| t.get("name")?.as_str())
            {
                names.insert(name.to_owned());
                names.insert(pypi_key(name));
            }
        }
    }
    if crate::npm::is_package(target) {
        for manifest in crate::npm::manifests(root, target).unwrap_or_default() {
            let name = fs::read_to_string(root.join(manifest))
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                .and_then(|parsed| Some(parsed.get("name")?.as_str()?.to_owned()));
            names.extend(name);
        }
    }
    names
}

/// The command lines of a manifest's tasks or scripts, each with its line,
/// and the names of them all, which runners such as `npm run` run.
#[derive(Debug, Default)]
pub(super) struct Tasks {
    pub(super) commands: Vec<(usize, Words)>,
    pub(super) names: BTreeSet<String>,
}

/// The Pixi tasks of `target`'s manifest: under `tasks`, `feature.*.tasks`,
/// `target.*.tasks` and `feature.*.target.*.tasks`, as a string or a `cmd`
/// string or list.
pub(super) fn pixi_tasks(root: &Path, target: &Target) -> Tasks {
    let Ok(text) = fs::read_to_string(root.join(&target.manifest)) else {
        return Tasks::default();
    };
    let Ok(parsed) = text.parse::<toml::Table>() else {
        return Tasks::default();
    };
    let (pixi, prefix) = if target
        .manifest
        .file_name()
        .is_some_and(|n| n == "pyproject.toml")
    {
        let pixi = parsed
            .get("tool")
            .and_then(|t| t.get("pixi"))
            .and_then(toml::Value::as_table);
        (pixi, "tool.pixi.")
    } else {
        (Some(&parsed), "")
    };
    let Some(pixi) = pixi else {
        return Tasks::default();
    };
    let mut task_tables = tasks_of(pixi, prefix.to_owned());
    if let Some(features) = pixi.get("feature").and_then(toml::Value::as_table) {
        for (name, feature) in features {
            if let Some(feature) = feature.as_table() {
                task_tables.extend(tasks_of(feature, format!("{prefix}feature.{name}.")));
            }
        }
    }
    let mut tasks = Tasks::default();
    for (path, table) in task_tables {
        for (name, task) in table {
            tasks.names.insert(name.clone());
            let command: Option<Words> = match task {
                toml::Value::String(line) => Some(words(line)),
                toml::Value::Table(task) => match task.get("cmd") {
                    Some(toml::Value::String(line)) => Some(words(line)),
                    Some(toml::Value::Array(tokens)) => Some(
                        tokens
                            .iter()
                            .map(|t| t.as_str().map(str::to_owned))
                            .collect(),
                    ),
                    _ => None,
                },
                _ => None,
            };
            if let Some(command) = command {
                tasks
                    .commands
                    .push((task_line(&text, &path, name), command));
            }
        }
    }
    tasks.commands.sort_by_key(|(line, _)| *line);
    tasks
}

/// The task tables of a Pixi manifest or feature `table` at TOML path
/// `prefix`, each with its path: its own, and those of its platform
/// targets.
fn tasks_of(table: &toml::Table, prefix: String) -> Vec<(String, &toml::Table)> {
    let mut found = vec![];
    if let Some(tasks) = table.get("tasks").and_then(toml::Value::as_table) {
        found.push((format!("{prefix}tasks"), tasks));
    }
    if let Some(targets) = table.get("target").and_then(toml::Value::as_table) {
        for (platform, target) in targets {
            if let Some(tasks) = target.get("tasks").and_then(toml::Value::as_table) {
                found.push((format!("{prefix}target.{platform}.tasks"), tasks));
            }
        }
    }
    found
}

/// The 1-based line task `name` of the tasks table at TOML path `path` is
/// on: its own `[path.name]` table, else its key below the `[path]` table,
/// else the first line naming it.
fn task_line(text: &str, path: &str, name: &str) -> usize {
    let header = |line: &str| line.trim().starts_with('[');
    let lines: Vec<&str> = text.lines().collect();
    let own = [format!("[{path}.{name}]"), format!("[{path}.\"{name}\"]")];
    if let Some(index) = lines.iter().position(|l| own.iter().any(|h| l.trim() == h)) {
        return index + 1;
    }
    let is_key = |line: &str| {
        let line = line.trim_start();
        [name.to_owned(), format!("\"{name}\"")].iter().any(|key| {
            line.strip_prefix(key.as_str())
                .is_some_and(|rest| rest.trim_start().starts_with('='))
        })
    };
    if let Some(start) = lines.iter().position(|l| l.trim() == format!("[{path}]")) {
        let within = lines[start + 1..]
            .iter()
            .take_while(|l| !header(l))
            .position(|l| is_key(l));
        if let Some(offset) = within {
            return start + offset + 2;
        }
    }
    lines
        .iter()
        .position(|l| is_key(l))
        .map_or(1, |index| index + 1)
}

/// The scripts of `manifest`, a `package.json` relative to `root`.
pub(super) fn package_scripts(root: &Path, manifest: &Path) -> Tasks {
    let Ok(text) = fs::read_to_string(root.join(manifest)) else {
        return Tasks::default();
    };
    let Ok(parsed) = serde_json::from_str::<Value>(text.strip_prefix('\u{feff}').unwrap_or(&text))
    else {
        return Tasks::default();
    };
    let lines: Vec<&str> = text.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.contains("\"scripts\""))
        .unwrap_or(0);
    let mut tasks = Tasks::default();
    for (name, command) in parsed
        .get("scripts")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        tasks.names.insert(name.clone());
        let Some(command) = command.as_str() else {
            continue;
        };
        // The key as JSON writes it, escapes included.
        let key = serde_json::to_string(name).unwrap_or_default();
        let line = lines[start..]
            .iter()
            .position(|l| {
                l.trim_start()
                    .strip_prefix(key.as_str())
                    .is_some_and(|rest| rest.trim_start().starts_with(':'))
            })
            .map_or(start + 1, |offset| start + offset + 1);
        tasks.commands.push((line, words(command)));
    }
    tasks.commands.sort_by_key(|(line, _)| *line);
    tasks
}

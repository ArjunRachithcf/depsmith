//! The lock change between two resolutions of a target, and the packages it
//! affects through the lock graph.
use super::tables::{is_build_tool, INTERPRETERS, PYTHON_STDLIB_MAP};
use crate::{
    adapter::Adapter,
    constraints::Declaration,
    graph::{identity, Id, LockGraph},
    LockPlatform, Package, Result, Target,
};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::{cmp::Ordering, collections::BTreeMap};

/// How a package differs between two resolutions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeKind {
    /// Its version or source changed.
    Updated,
    /// It is new.
    Added,
    /// It is gone.
    Removed,
}

/// A package a lock change moves, adds or removes on one platform.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockChange {
    /// Ecosystem of the package, such as `conda` or `pypi`.
    pub ecosystem: String,
    /// Package name as the lock spells it ("after" side, unless removed).
    pub name: String,
    /// Platform the package is resolved for, such as `linux-64`.
    pub platform: String,
    /// The environment it changed in, where the lock has several (Pixi).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    /// The version before; `None` when added, or when the lock records none.
    pub before: Option<String>,
    /// The version after; `None` when removed, or when the lock records none.
    pub after: Option<String>,
    /// Whether it was updated, added or removed.
    pub kind: ChangeKind,
    /// Whether the first version component, or below 1.0 the leading `0.x`
    /// components, differ, as the ecosystem's version scheme compares them.
    pub major: bool,
    /// Whether the target declares the package.
    pub direct: bool,
}

/// The lock change from `before` to `after`, two inventories of one target,
/// with `declarations` (read from the "after" side) deciding which changes
/// are direct.
///
/// Inventories rather than lock graphs are compared because only they record
/// every lock's versions and sources: a Pixi lock graph has neither. Packages
/// are paired by ecosystem, normalized name, platform and environment, so an
/// environment that catches up to another's version is updated there, and
/// one that drops a package another keeps removes it there. A package locked
/// at several versions within one environment and platform keeps a version
/// that disappears as an update to a remaining one, never a removal.
pub fn lock_changes<'a>(
    before: &'a [Package],
    after: &'a [Package],
    declarations: &[Declaration],
) -> Vec<LockChange> {
    let mut groups: BTreeMap<Key, Locked<'a>> = BTreeMap::new();
    for package in before {
        for key in keys(package) {
            groups.entry(key).or_default().0.push(package);
        }
    }
    for package in after {
        for key in keys(package) {
            groups.entry(key).or_default().1.push(package);
        }
    }
    let mut changes = vec![];
    for ((id, platform, environment), (old, new)) in groups {
        let direct = is_declared(declarations, &id);
        let kept =
            |side: &[&Package], package: &Package| side.iter().any(|p| p.same_resolution(package));
        let mut left: Vec<&Package> = vec![];
        let mut right: Vec<&Package> = new.iter().filter(|p| !kept(&old, p)).copied().collect();
        // A version locked from another source is an update of that version.
        let mut pairs = vec![];
        for package in old.iter().filter(|p| !kept(&new, p)) {
            match right.iter().position(|p| p.version == package.version) {
                Some(i) => pairs.push((Some(*package), Some(right.remove(i)))),
                None => left.push(package),
            }
        }
        let order = |a: &Package, b: &Package| compare(&id.0, &a.version, &b.version);
        left.sort_by(|a, b| order(a, b));
        right.sort_by(|a, b| order(a, b));
        let paired = left.len().min(right.len());
        for (from, to) in left.iter().zip(&right) {
            pairs.push((Some(*from), Some(*to)));
        }
        let newest = |packages: &[&'a Package]| packages.iter().copied().max_by(|a, b| order(a, b));
        for from in &left[paired..] {
            pairs.push((Some(*from), newest(&new)));
        }
        for to in &right[paired..] {
            pairs.push((newest(&old), Some(*to)));
        }
        for (from, to) in pairs {
            let package = to.or(from).expect("a pair has a side");
            let (before, after) = (from.and_then(version), to.and_then(version));
            changes.push(LockChange {
                ecosystem: package.ecosystem.clone(),
                name: package.name.clone(),
                platform: platform.clone(),
                environment: environment.clone(),
                kind: match (from, to) {
                    (Some(_), Some(_)) => ChangeKind::Updated,
                    (None, _) => ChangeKind::Added,
                    (_, None) => ChangeKind::Removed,
                },
                major: match (&before, &after) {
                    (Some(from), Some(to)) => major(&id.0, from, to),
                    _ => false,
                },
                before,
                after,
                direct,
            });
        }
    }
    changes
}

/// A package identity, its ecosystem and normalized name, with a platform
/// and, where the lock has several, an environment.
type Key = (Id, String, Option<String>);

/// The packages one key locks, before and after.
type Locked<'a> = (Vec<&'a Package>, Vec<&'a Package>);

/// The keys `package` is locked under: one per environment that locks it.
fn keys(package: &Package) -> Vec<Key> {
    let id = identity(&package.ecosystem, &package.name);
    if package.environments.is_empty() {
        return vec![(id, package.platform.clone(), None)];
    }
    package
        .environments
        .iter()
        .map(|e| (id.clone(), package.platform.clone(), Some(e.clone())))
        .collect()
}

/// An affected package's platform, normalized name and identity, ordering
/// affected packages by platform and name.
type AffectedKey = (String, String, Id);

/// The affected package `(ecosystem, name)` on `platform` in `packages`,
/// added unchanged and unreached when new.
fn entry<'m>(
    packages: &'m mut BTreeMap<AffectedKey, AffectedPackage>,
    declarations: &[Declaration],
    platform: &str,
    (ecosystem, name): (&str, &str),
) -> &'m mut AffectedPackage {
    let id = identity(ecosystem, name);
    packages
        .entry((platform.to_owned(), id.1.clone(), id.clone()))
        .or_insert_with(|| AffectedPackage {
            declared: is_declared(declarations, &id),
            ecosystem: ecosystem.to_owned(),
            name: name.to_owned(),
            platform: platform.to_owned(),
            changed: false,
            reaches: vec![],
            stdlib: vec![],
        })
}

/// Whether `declarations` declare the package `id`.
fn is_declared(declarations: &[Declaration], id: &Id) -> bool {
    declarations
        .iter()
        .any(|d| identity(&d.ecosystem, &d.package) == *id)
}

/// The package's version, unless the lock records none.
fn version(package: &Package) -> Option<String> {
    let version = package.version.trim();
    (!version.is_empty() && version != "unknown").then(|| version.to_owned())
}

/// `left` and `right` ordered by `ecosystem`'s version scheme, or as text.
fn compare(ecosystem: &str, left: &str, right: &str) -> Ordering {
    match crate::ecosystem::scheme(ecosystem) {
        Some(scheme) => scheme.compare(left, right),
        None => left.cmp(right),
    }
}

/// Whether moving from `before` to `after` changes the major version.
fn major(ecosystem: &str, before: &str, after: &str) -> bool {
    let (before, after) = (
        crate::ecosystem::major_part(before),
        crate::ecosystem::major_part(after),
    );
    before != after && compare(ecosystem, &before, &after) != Ordering::Equal
}

/// One side of a comparison: a target's inventory and lock graph, as the
/// adapter's `inventory` and `lock_graph` read them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lock {
    /// The resolved packages, with versions and sources.
    pub inventory: Vec<Package>,
    /// What each resolved package requires; empty when the adapter reads no
    /// lock graph.
    pub graph: LockGraph,
    /// The platforms the lock names, where it names them (Pixi lock v7).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub platforms: Vec<LockPlatform>,
}

impl Lock {
    /// One side of a comparison: `target`'s inventory and lock graph under
    /// `root`, as `adapter` reads them. A side with none of the target's
    /// lock files is an empty lock, so locking for the first time adds every
    /// package.
    ///
    /// # Errors
    ///
    /// Returns the adapter's error when a lock exists but cannot be read.
    pub fn read(adapter: &dyn Adapter, root: &Path, target: &Target) -> Result<Self> {
        let locks = super::compare::lock_files(adapter, target);
        if !locks.is_empty() && !locks.iter().any(|lock| root.join(lock).is_file()) {
            return Ok(Self::default());
        }
        Ok(Self {
            inventory: adapter.inventory(root, target)?,
            graph: adapter.lock_graph(root, target)?.unwrap_or_default(),
            platforms: adapter.lock_platforms(root, target)?,
        })
    }

    /// This lock with its platforms renamed to those of `after` that are the
    /// same platform under another name. A name both sides give the same
    /// platform stays; the others pair by subdir and virtual packages, then
    /// by subdir alone, where each side has only one such platform left.
    fn renamed_to(&self, after: &Lock) -> Lock {
        let kept = |p: &LockPlatform, other: &Lock| other.platforms.contains(p);
        let mut before_left: Vec<&LockPlatform> =
            self.platforms.iter().filter(|p| !kept(p, after)).collect();
        let mut after_left: Vec<&LockPlatform> =
            after.platforms.iter().filter(|p| !kept(p, self)).collect();
        let mut renames: BTreeMap<String, String> = BTreeMap::new();
        for with_virtual_packages in [true, false] {
            let key = |p: &LockPlatform| {
                let virtual_packages = if with_virtual_packages {
                    p.virtual_packages.clone()
                } else {
                    vec![]
                };
                (p.subdir.clone(), virtual_packages)
            };
            let pairs: Vec<(&LockPlatform, &LockPlatform)> = before_left
                .iter()
                .filter_map(|old| {
                    let k = key(old);
                    let mut olds = before_left.iter().filter(|p| key(p) == k);
                    let mut news = after_left.iter().filter(|p| key(p) == k);
                    match (olds.next(), olds.next(), news.next(), news.next()) {
                        (Some(_), None, Some(new), None) => Some((*old, *new)),
                        _ => None,
                    }
                })
                .collect();
            for (old, new) in pairs {
                before_left.retain(|p| *p != old);
                after_left.retain(|p| *p != new);
                renames.insert(old.name.clone(), new.name.clone());
            }
        }
        // A platform left unpaired whose name another now takes compares as
        // a platform of its own.
        for old in before_left {
            if renames.values().any(|new| *new == old.name) {
                renames.insert(old.name.clone(), format!("{} (before)", old.name));
            }
        }
        let mut renamed = self.clone();
        let rename = |platform: &mut String| {
            if let Some(new) = renames.get(platform.as_str()) {
                platform.clone_from(new);
            }
        };
        renamed
            .inventory
            .iter_mut()
            .for_each(|p| rename(&mut p.platform));
        renamed
            .graph
            .nodes
            .iter_mut()
            .for_each(|n| rename(&mut n.platform));
        renamed
            .platforms
            .iter_mut()
            .for_each(|p| rename(&mut p.name));
        renamed
    }
}

/// A change an affected package reaches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reached {
    /// The changed package, as its lock change names it.
    pub change: String,
    /// The shortest chain of packages from the affected package down to the
    /// changed one; just the package for the changed package itself.
    pub path: Vec<String>,
}

/// A package a lock change moves, adds or removes, or one whose dependency
/// path reaches such a package, on one platform.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AffectedPackage {
    /// Ecosystem of the package.
    pub ecosystem: String,
    /// The package name.
    pub name: String,
    /// Platform it is resolved for.
    pub platform: String,
    /// Whether the target declares it.
    pub declared: bool,
    /// Whether the lock change moves, adds or removes it.
    pub changed: bool,
    /// The changes it reaches.
    pub reaches: Vec<Reached>,
    /// The Python standard-library modules built on it, for a changed native
    /// library of the interpreter.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stdlib: Vec<String>,
}

/// Why every test must run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "kebab-case")]
pub enum FullRun {
    /// The interpreter itself changed.
    Interpreter {
        /// The interpreter package, such as `python`.
        name: String,
        /// Platform it changed on.
        platform: String,
        /// Its version before, where known.
        before: Option<String>,
        /// Its version after, where known.
        after: Option<String>,
    },
    /// A compiler or a build tool of native extensions changed (see
    /// [`BUILD_TOOLS`](super::BUILD_TOOLS)).
    BuildTool {
        /// Ecosystem of the tool.
        ecosystem: String,
        /// The tool package, such as `cython`.
        name: String,
        /// Platform it changed on.
        platform: String,
    },
    /// A platform's virtual packages changed, such as the CUDA version or
    /// the glibc it assumes, which no package change shows.
    VirtualPackages {
        /// The platform, by its "after" name.
        platform: String,
        /// Its virtual packages before, sorted.
        before: Vec<String>,
        /// Its virtual packages after, sorted.
        after: Vec<String>,
    },
}

/// The impact of a lock change on the packages of a target.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Affected {
    /// The lock change.
    pub changes: Vec<LockChange>,
    /// The affected packages, by platform and name.
    pub packages: Vec<AffectedPackage>,
    /// Why every test must run, if any reason applies.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub full_run: Vec<FullRun>,
}

/// The lock change from `before` to `after` (see [`lock_changes`]) and the
/// packages it affects: each changed package, and every package that depends
/// on the version that changed, found by walking the lock graph in reverse
/// per platform ("after" side, or "before" for a removal). `declarations`
/// come from the "after" side.
///
/// A platform the "before" lock names differently is compared under its
/// "after" name: one with the same subdir and virtual packages, else the only
/// platform of its subdir left unpaired on each side.
///
/// The walk never passes a conda package named in `barriers`, such as
/// [`RUNTIME_BARRIERS`](super::RUNTIME_BARRIERS) for Pixi, nor a conda
/// interpreter ([`INTERPRETERS`](super::INTERPRETERS)), so a change below
/// `python` does not reach everything that requires `python`. A change to an
/// interpreter is a full run instead, as is a change to a compiler or a build
/// tool of native extensions or to a platform's virtual packages (such as the
/// CUDA version it assumes), and a change to one of the interpreter's native
/// libraries names the standard-library modules built on it.
pub fn affected(
    before: &Lock,
    after: &Lock,
    declarations: &[Declaration],
    barriers: &[&str],
) -> Affected {
    let before = &before.renamed_to(after);
    let changes = lock_changes(&before.inventory, &after.inventory, declarations);
    let barriers: Vec<(&str, &str)> = barriers
        .iter()
        .chain(INTERPRETERS)
        .map(|name| ("conda", *name))
        .collect();
    let mut packages: BTreeMap<AffectedKey, AffectedPackage> = BTreeMap::new();
    let mut full_run = vec![];
    for change in &changes {
        let conda = change.ecosystem == "conda";
        if conda && INTERPRETERS.contains(&change.name.as_str()) {
            full_run.push(FullRun::Interpreter {
                name: change.name.clone(),
                platform: change.platform.clone(),
                before: change.before.clone(),
                after: change.after.clone(),
            });
        }
        if is_build_tool(&change.name) {
            full_run.push(FullRun::BuildTool {
                ecosystem: change.ecosystem.clone(),
                name: change.name.clone(),
                platform: change.platform.clone(),
            });
        }
        let (graph, version) = if change.kind == ChangeKind::Removed {
            (&before.graph, change.before.as_deref())
        } else {
            (&after.graph, change.after.as_deref())
        };
        let package = entry(
            &mut packages,
            declarations,
            &change.platform,
            (&change.ecosystem, &change.name),
        );
        package.changed = true;
        if conda {
            package.stdlib = PYTHON_STDLIB_MAP
                .iter()
                .filter(|(library, _)| *library == change.name)
                .flat_map(|(_, modules)| modules.iter().map(|m| m.to_string()))
                .collect();
        }
        package.reaches.push(Reached {
            change: change.name.clone(),
            path: vec![change.name.clone()],
        });
        let dependents = graph.dependents(
            &change.platform,
            (&change.ecosystem, &change.name),
            version,
            &barriers,
        );
        for dependent in dependents {
            entry(
                &mut packages,
                declarations,
                &change.platform,
                (&dependent.ecosystem, &dependent.name),
            )
            .reaches
            .push(Reached {
                change: change.name.clone(),
                path: dependent.path,
            });
        }
    }
    for old in &before.platforms {
        if let Some(new) = after.platforms.iter().find(|new| new.name == old.name) {
            if new.virtual_packages != old.virtual_packages {
                full_run.push(FullRun::VirtualPackages {
                    platform: new.name.clone(),
                    before: old.virtual_packages.clone(),
                    after: new.virtual_packages.clone(),
                });
            }
        }
    }
    Affected {
        changes,
        packages: packages.into_values().collect(),
        full_run,
    }
}

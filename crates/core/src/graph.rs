//! Lock graphs: the packages a target's lock resolves, per platform, with
//! what each requires, and the dependency paths from declared dependencies
//! to any of them.
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

/// The packages a lock resolves, with their requirements.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockGraph {
    /// One node per resolved package, platform and environment.
    pub nodes: Vec<Node>,
}

/// A resolved package on one platform.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    /// Ecosystem of the package identity, such as `conda` or `pypi`.
    pub ecosystem: String,
    /// Package name as the lock spells it.
    pub name: String,
    /// The locked version, where the graph records versions (Cargo.lock,
    /// package-lock.json); a requirement naming a version reaches only that
    /// version's node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Platform the package was resolved for, such as `linux-64`.
    pub platform: String,
    /// The packages it requires, from its own ecosystem.
    pub requires: Vec<Requirement>,
}

/// A package a node requires.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requirement {
    /// Name of the required package.
    pub name: String,
    /// The version requirement, where the lock records one.
    pub spec: Option<String>,
    /// The exact locked version this requirement resolved to, where the lock
    /// names it. It reaches only that version's node, or every version of the
    /// package when no node records that version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// A package identity in a graph: its ecosystem and normalized name.
pub(crate) type Id = (String, String);

/// `name` as `ecosystem` compares names: PyPI ignores case and separators,
/// other ecosystems only case.
pub(crate) fn identity(ecosystem: &str, name: &str) -> Id {
    let key = if ecosystem == "pypi" {
        crate::adapter::pypi_key(name)
    } else {
        name.trim().to_ascii_lowercase()
    };
    (ecosystem.to_owned(), key)
}

impl LockGraph {
    /// The shortest dependency path from each of `roots` (ecosystem and
    /// name) that reaches the package `name` of `ecosystem` on `platform`;
    /// see [`Reach`] for answering many packages.
    pub fn paths_to(
        &self,
        platform: &str,
        roots: &[(String, String)],
        ecosystem: &str,
        name: &str,
    ) -> Vec<Vec<String>> {
        self.reach(platform, roots).paths_to(ecosystem, name)
    }

    /// Everything `roots` (ecosystem and name) reach on `platform`. A
    /// requirement resolves to a package of its node's ecosystem, else to
    /// one of another ecosystem with that name, so a PyPI requirement can be
    /// satisfied by a conda package. A requirement naming a locked version
    /// reaches only that version; otherwise it reaches every locked version
    /// of the package. A package locked in several environments keeps every
    /// requirement. Roots and reached packages are matched by name.
    pub fn reach(&self, platform: &str, roots: &[(String, String)]) -> Reach {
        let edges = Edges::new(self, platform);
        let searches = roots
            .iter()
            .map(|(ecosystem, name)| {
                let start = identity(ecosystem, name);
                let mut seen: HashMap<Vertex, Step> = HashMap::new();
                let mut queue = VecDeque::new();
                for vertex in edges.vertices(start.clone()) {
                    let root = Step {
                        previous: None,
                        spelled: name.clone(),
                        depth: 0,
                    };
                    seen.insert(vertex.clone(), root);
                    queue.push_back(vertex);
                }
                while let Some(current) = queue.pop_front() {
                    let depth = seen[&current].depth;
                    for requirement in edges.requires.get(&current).into_iter().flatten() {
                        for next in edges.resolve(&current.id.0, requirement) {
                            if !seen.contains_key(&next) {
                                let step = Step {
                                    previous: Some(current.clone()),
                                    spelled: requirement.name.clone(),
                                    depth: depth + 1,
                                };
                                seen.insert(next.clone(), step);
                                queue.push_back(next);
                            }
                        }
                    }
                }
                (start, seen)
            })
            .collect();
        Reach { searches }
    }

    /// Every package that depends on the package `name` of `ecosystem` on
    /// `platform`, directly or through others, each with the shortest chain
    /// of packages from it down to that package, nearest first. With a
    /// `version` the graph locks, only that version's dependents count.
    /// Requirements resolve as in [`LockGraph::reach`]. The walk never passes
    /// a package in `barriers` (ecosystem and name): a barrier that is the
    /// package itself has no dependents, and one reached on the way up is
    /// neither reported nor walked past.
    pub fn dependents(
        &self,
        platform: &str,
        (ecosystem, name): (&str, &str),
        version: Option<&str>,
        barriers: &[(&str, &str)],
    ) -> Vec<Dependent> {
        let edges = Edges::new(self, platform);
        let barrier = |id: &Id| barriers.iter().any(|(e, b)| identity(e, b) == *id);
        let start = identity(ecosystem, name);
        if barrier(&start) {
            return vec![];
        }
        let mut required_by: HashMap<Vertex, Vec<(&Vertex, &str)>> = HashMap::new();
        for (vertex, requires) in &edges.requires {
            for requirement in requires {
                for next in edges.resolve(&vertex.id.0, requirement) {
                    required_by
                        .entry(next)
                        .or_default()
                        .push((vertex, requirement.name.as_str()));
                }
            }
        }
        for parents in required_by.values_mut() {
            parents.sort();
        }
        // Each reached package, with the package below it on the path and
        // how its requirement spells that package.
        let mut below: HashMap<Vertex, Option<(Vertex, &str)>> = HashMap::new();
        let mut queue = VecDeque::new();
        let locked = edges.vertices(start.clone());
        let changed: Vec<Vertex> = locked
            .iter()
            .filter(|v| version.is_some() && v.version.as_deref() == version)
            .cloned()
            .collect();
        for vertex in if changed.is_empty() { locked } else { changed } {
            below.insert(vertex.clone(), None);
            queue.push_back(vertex);
        }
        let mut order = vec![];
        while let Some(current) = queue.pop_front() {
            for &(parent, spelled) in required_by.get(&current).into_iter().flatten() {
                if below.contains_key(parent) || barrier(&parent.id) {
                    continue;
                }
                below.insert(parent.clone(), Some((current.clone(), spelled)));
                queue.push_back(parent.clone());
                order.push(parent.clone());
            }
        }
        let mut reported: Vec<Id> = vec![];
        let mut result = vec![];
        for vertex in order {
            if vertex.id == start || reported.contains(&vertex.id) {
                continue;
            }
            reported.push(vertex.id.clone());
            let spelled = edges
                .spelled
                .get(&vertex.id)
                .copied()
                .unwrap_or(&vertex.id.1);
            let mut path = vec![spelled.to_owned()];
            let mut current = &vertex;
            while let Some(Some((next, spelled))) = below.get(current) {
                path.push((*spelled).to_owned());
                current = next;
            }
            result.push(Dependent {
                ecosystem: vertex.id.0.clone(),
                name: spelled.to_owned(),
                version: vertex.version.clone(),
                path,
            });
        }
        result
    }
}

/// A package that depends on another, found by [`LockGraph::dependents`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dependent {
    /// Ecosystem of the package.
    pub ecosystem: String,
    /// The package as its node spells it.
    pub name: String,
    /// Its locked version, where the graph records versions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The shortest chain of packages from it down to the package depended
    /// on, spelled as the requirements along it spell the packages.
    pub path: Vec<String>,
}

/// One platform's lock graph, indexed for searching in either direction.
struct Edges<'a> {
    /// What each locked package requires, once per name and locked version.
    requires: HashMap<Vertex, Vec<&'a Requirement>>,
    /// The locked versions of each package, sorted.
    versions: HashMap<Id, Vec<Vertex>>,
    /// Each package as its node spells it.
    spelled: HashMap<Id, &'a str>,
}

impl<'a> Edges<'a> {
    fn new(graph: &'a LockGraph, platform: &str) -> Self {
        let mut requires: HashMap<Vertex, Vec<&Requirement>> = HashMap::new();
        let mut spelled = HashMap::new();
        for node in graph.nodes.iter().filter(|n| n.platform == platform) {
            let id = identity(&node.ecosystem, &node.name);
            spelled.entry(id.clone()).or_insert(node.name.as_str());
            let requires = requires
                .entry(Vertex {
                    id,
                    version: node.version.clone(),
                })
                .or_default();
            for requirement in &node.requires {
                if !requires
                    .iter()
                    .any(|r| r.name == requirement.name && r.version == requirement.version)
                {
                    requires.push(requirement);
                }
            }
        }
        let mut versions: HashMap<Id, Vec<Vertex>> = HashMap::new();
        for vertex in requires.keys() {
            versions
                .entry(vertex.id.clone())
                .or_default()
                .push(vertex.clone());
        }
        for locked in versions.values_mut() {
            locked.sort();
        }
        Self {
            requires,
            versions,
            spelled,
        }
    }

    /// The locked versions of `id`, or the unversioned package when none is
    /// locked.
    fn vertices(&self, id: Id) -> Vec<Vertex> {
        self.versions
            .get(&id)
            .cloned()
            .unwrap_or_else(|| vec![Vertex { id, version: None }])
    }

    /// The packages `requirement` of a package of ecosystem `from` resolves
    /// to.
    fn resolve(&self, from: &str, requirement: &Requirement) -> Vec<Vertex> {
        let own = identity(from, &requirement.name);
        let id = if self.versions.contains_key(&own) {
            own
        } else {
            self.versions
                .keys()
                .find(|(ecosystem, _)| {
                    ecosystem != from
                        && self
                            .versions
                            .contains_key(&identity(ecosystem, &requirement.name))
                })
                .map_or(own, |(ecosystem, _)| identity(ecosystem, &requirement.name))
        };
        let all = self.vertices(id);
        let exact: Vec<Vertex> = all
            .iter()
            .filter(|vertex| requirement.version.is_some() && vertex.version == requirement.version)
            .cloned()
            .collect();
        if exact.is_empty() {
            all
        } else {
            exact
        }
    }
}

/// A package in a search: its identity and, where the graph records one,
/// its version.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Vertex {
    id: Id,
    version: Option<String>,
}

/// How a search reached a package.
struct Step {
    /// The package before it on the path, none for a root.
    previous: Option<Vertex>,
    /// The package as the requirement along the path spells it.
    spelled: String,
    /// The path's length.
    depth: usize,
}

/// One root's search: its identity, and how it reached each package.
type Search = (Id, HashMap<Vertex, Step>);

/// What declared dependencies reach in one platform's lock graph, for
/// finding the dependency paths to many packages.
pub struct Reach {
    /// One search per root, in the order of the roots.
    searches: Vec<Search>,
}

impl Reach {
    /// The shortest dependency path from each root that reaches the package
    /// `name` of `ecosystem`, at any locked version, in the order of the
    /// roots, each spelled as the requirements along it spell the packages.
    /// A root never explains itself.
    pub fn paths_to(&self, ecosystem: &str, name: &str) -> Vec<Vec<String>> {
        let target = identity(ecosystem, name);
        let mut paths = vec![];
        for (start, seen) in &self.searches {
            if *start == target {
                continue;
            }
            let Some(end) = seen
                .iter()
                .filter(|(vertex, _)| vertex.id == target)
                .min_by(|(a, x), (b, y)| x.depth.cmp(&y.depth).then_with(|| a.cmp(b)))
                .map(|(vertex, _)| vertex.clone())
            else {
                continue;
            };
            let mut path = vec![];
            let mut current = Some(end);
            while let Some(vertex) = current {
                let step = &seen[&vertex];
                path.push(step.spelled.clone());
                current = step.previous.clone();
            }
            path.reverse();
            paths.push(path);
        }
        paths
    }
}

/// Set the introducers and dependency paths of each of `changes` whose
/// package `roots` (ecosystem and name) do not declare: from `after`, or
/// from `before` for a removed package. Each platform's graph is searched
/// once.
pub(crate) fn explain(
    changes: &mut [crate::DependencyChange],
    before: &LockGraph,
    after: &LockGraph,
    roots: &[(String, String)],
) {
    let declared: Vec<Id> = roots.iter().map(|(e, n)| identity(e, n)).collect();
    let mut reaches: HashMap<(bool, String), Reach> = HashMap::new();
    for change in changes {
        let (package, removed) = match (&change.after, &change.before) {
            (Some(package), _) => (package, false),
            (None, Some(package)) => (package, true),
            (None, None) => continue,
        };
        if declared.contains(&identity(&package.ecosystem, &package.name)) {
            continue;
        }
        let reach = reaches
            .entry((removed, package.platform.clone()))
            .or_insert_with(|| {
                if removed { before } else { after }.reach(&package.platform, roots)
            });
        change.paths = reach.paths_to(&package.ecosystem, &package.name);
        change.introducers = change.paths.iter().map(|p| p[0].clone()).collect();
    }
}

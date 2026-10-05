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
}

/// A package identity in a graph: its ecosystem and normalized name.
type Id = (String, String);

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
    /// satisfied by a conda package. A package locked in several
    /// environments keeps every requirement.
    pub fn reach(&self, platform: &str, roots: &[(String, String)]) -> Reach {
        let mut edges: HashMap<Id, Vec<&Requirement>> = HashMap::new();
        for node in self.nodes.iter().filter(|n| n.platform == platform) {
            let requires = edges
                .entry(identity(&node.ecosystem, &node.name))
                .or_default();
            for requirement in &node.requires {
                if !requires.iter().any(|r| r.name == requirement.name) {
                    requires.push(requirement);
                }
            }
        }
        let resolve = |from: &str, name: &str| -> Id {
            let own = identity(from, name);
            if edges.contains_key(&own) {
                return own;
            }
            edges
                .keys()
                .find(|(ecosystem, _)| {
                    ecosystem != from && edges.contains_key(&identity(ecosystem, name))
                })
                .map_or(own, |(ecosystem, _)| identity(ecosystem, name))
        };
        let searches = roots
            .iter()
            .map(|(ecosystem, name)| {
                let start = identity(ecosystem, name);
                let mut seen: HashMap<Id, (Option<Id>, String)> =
                    HashMap::from([(start.clone(), (None, name.clone()))]);
                let mut queue = VecDeque::from([start.clone()]);
                while let Some(current) = queue.pop_front() {
                    for requirement in edges.get(&current).into_iter().flatten() {
                        let next = resolve(&current.0, &requirement.name);
                        if !seen.contains_key(&next) {
                            seen.insert(
                                next.clone(),
                                (Some(current.clone()), requirement.name.clone()),
                            );
                            queue.push_back(next);
                        }
                    }
                }
                (start, seen)
            })
            .collect();
        Reach { searches }
    }
}

/// One root's search: its identity, and each reached package's predecessor
/// and spelling.
type Search = (Id, HashMap<Id, (Option<Id>, String)>);

/// What declared dependencies reach in one platform's lock graph, for
/// finding the dependency paths to many packages.
pub struct Reach {
    /// One search per root, in the order of the roots.
    searches: Vec<Search>,
}

impl Reach {
    /// The shortest dependency path from each root that reaches the package
    /// `name` of `ecosystem`, in the order of the roots, each spelled as the
    /// requirements along it spell the packages. A root never explains
    /// itself.
    pub fn paths_to(&self, ecosystem: &str, name: &str) -> Vec<Vec<String>> {
        let target = identity(ecosystem, name);
        let mut paths = vec![];
        for (start, seen) in &self.searches {
            if *start == target || !seen.contains_key(&target) {
                continue;
            }
            let mut path = vec![];
            let mut current = Some(target.clone());
            while let Some(id) = current {
                let (previous, spelled) = &seen[&id];
                path.push(spelled.clone());
                current = previous.clone();
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

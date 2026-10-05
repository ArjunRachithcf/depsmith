//! Lock graphs: the packages a target's lock resolves, per platform, with
//! what each requires, and the dependency paths from declared dependencies
//! to any of them.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

/// The packages a lock resolves, with their requirements.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockGraph {
    /// One node per resolved package and platform.
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
    /// The packages it requires.
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

/// `name` compared across case and `-`, `_` and `.` separators.
pub(crate) fn key(name: &str) -> String {
    name.to_ascii_lowercase().replace(['_', '.'], "-")
}

impl LockGraph {
    /// The shortest dependency path from each of `roots` that reaches `name`
    /// on `platform`, in the order of `roots`. Each path starts at the root
    /// and ends at `name`, spelled as the requirements along it spell them.
    pub fn paths_to(&self, platform: &str, roots: &[String], name: &str) -> Vec<Vec<String>> {
        let edges: BTreeMap<String, &Node> = self
            .nodes
            .iter()
            .filter(|n| n.platform == platform)
            .map(|n| (key(&n.name), n))
            .collect();
        let target = key(name);
        let mut paths = vec![];
        for root in roots {
            if key(root) == target {
                continue;
            }
            let mut seen = vec![key(root)];
            let mut queue = VecDeque::from([vec![root.clone()]]);
            while let Some(path) = queue.pop_front() {
                let last = key(path.last().unwrap());
                if last == target {
                    paths.push(path);
                    break;
                }
                for requirement in edges.get(&last).map_or(&[][..], |n| &n.requires) {
                    let next = key(&requirement.name);
                    if !seen.contains(&next) {
                        seen.push(next);
                        let mut longer = path.clone();
                        longer.push(requirement.name.clone());
                        queue.push_back(longer);
                    }
                }
            }
        }
        paths
    }
}

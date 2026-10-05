//! Dependency paths through a lock graph: from each declared dependency to a
//! package, shortest first, per platform and ecosystem.
use depsmith_core::graph::{LockGraph, Node, Requirement};

fn node(ecosystem: &str, platform: &str, name: &str, requires: &[&str]) -> Node {
    Node {
        ecosystem: ecosystem.into(),
        name: name.into(),
        platform: platform.into(),
        requires: requires
            .iter()
            .map(|r| Requirement {
                name: (*r).into(),
                spec: None,
            })
            .collect(),
    }
}

fn conda(platform: &str, name: &str, requires: &[&str]) -> Node {
    node("conda", platform, name, requires)
}

fn root(ecosystem: &str, name: &str) -> (String, String) {
    (ecosystem.into(), name.into())
}

fn graph() -> LockGraph {
    LockGraph {
        nodes: vec![
            conda("linux-64", "scipy", &["numpy", "libblas"]),
            conda("linux-64", "matplotlib-base", &["numpy", "Pillow"]),
            conda("linux-64", "pillow", &["libjpeg"]),
            conda("linux-64", "libjpeg", &[]),
            conda("linux-64", "numpy", &["libblas"]),
            // A cycle back to numpy.
            conda("linux-64", "libblas", &["numpy"]),
            conda("win-64", "scipy", &[]),
        ],
    }
}

fn roots() -> Vec<(String, String)> {
    vec![root("conda", "scipy"), root("conda", "matplotlib-base")]
}

#[test]
fn each_declared_dependency_reaching_a_package_gives_its_shortest_path() {
    assert_eq!(
        graph().paths_to("linux-64", &roots(), "conda", "libblas"),
        [
            vec!["scipy", "libblas"],
            vec!["matplotlib-base", "numpy", "libblas"],
        ]
    );
}

#[test]
fn names_match_across_case() {
    assert_eq!(
        graph().paths_to("linux-64", &roots(), "conda", "libjpeg"),
        [vec!["matplotlib-base", "Pillow", "libjpeg"]]
    );
}

#[test]
fn other_platforms_and_unreached_packages_have_no_paths() {
    assert!(graph()
        .paths_to("win-64", &roots(), "conda", "libblas")
        .is_empty());
    assert!(graph()
        .paths_to("linux-64", &roots(), "conda", "absent")
        .is_empty());
}

#[test]
fn ecosystems_keep_their_own_packages_and_fall_back_to_each_other() {
    let graph = LockGraph {
        nodes: vec![
            // A PyPI package needing numpy, which conda provides.
            node("pypi", "linux-64", "my-app", &["numpy", "PyYAML"]),
            conda("linux-64", "numpy", &["libblas"]),
            conda("linux-64", "libblas", &[]),
            node("pypi", "linux-64", "pyyaml", &[]),
            // A conda package with the same name as a PyPI one.
            conda("linux-64", "pyyaml", &["yaml"]),
            conda("linux-64", "yaml", &[]),
        ],
    };
    let roots = [root("pypi", "my-app")];
    assert_eq!(
        graph.paths_to("linux-64", &roots, "conda", "libblas"),
        [vec!["my-app", "numpy", "libblas"]]
    );
    // my-app's PyYAML is the PyPI package, so conda's yaml is not reached.
    assert!(graph
        .paths_to("linux-64", &roots, "conda", "yaml")
        .is_empty());
}

#[test]
fn a_package_locked_in_several_environments_keeps_every_requirement() {
    let graph = LockGraph {
        nodes: vec![
            conda("linux-64", "app", &["lib"]),
            conda("linux-64", "lib", &["old"]),
            conda("linux-64", "lib", &["new"]),
            conda("linux-64", "old", &[]),
            conda("linux-64", "new", &[]),
        ],
    };
    let roots = [root("conda", "app")];
    assert_eq!(
        graph.paths_to("linux-64", &roots, "conda", "old"),
        [vec!["app", "lib", "old"]]
    );
    assert_eq!(
        graph.paths_to("linux-64", &roots, "conda", "new"),
        [vec!["app", "lib", "new"]]
    );
}

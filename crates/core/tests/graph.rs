//! Dependency paths through a lock graph: from each declared dependency to a
//! package, shortest first, per platform.
use depsmith_core::graph::{LockGraph, Node, Requirement};

fn node(platform: &str, name: &str, requires: &[&str]) -> Node {
    Node {
        ecosystem: "conda".into(),
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

fn graph() -> LockGraph {
    LockGraph {
        nodes: vec![
            node("linux-64", "scipy", &["numpy", "libblas"]),
            node("linux-64", "matplotlib-base", &["numpy", "Pillow"]),
            node("linux-64", "pillow", &["libjpeg"]),
            node("linux-64", "libjpeg", &[]),
            node("linux-64", "numpy", &["libblas"]),
            // A cycle back to numpy.
            node("linux-64", "libblas", &["numpy"]),
            node("win-64", "scipy", &[]),
        ],
    }
}

fn roots() -> Vec<String> {
    vec!["scipy".into(), "matplotlib-base".into()]
}

#[test]
fn each_declared_dependency_reaching_a_package_gives_its_shortest_path() {
    assert_eq!(
        graph().paths_to("linux-64", &roots(), "libblas"),
        [
            vec!["scipy", "libblas"],
            vec!["matplotlib-base", "numpy", "libblas"],
        ]
    );
}

#[test]
fn names_match_across_case_and_separators() {
    assert_eq!(
        graph().paths_to("linux-64", &roots(), "libjpeg"),
        [vec!["matplotlib-base", "Pillow", "libjpeg"]]
    );
}

#[test]
fn other_platforms_and_unreached_packages_have_no_paths() {
    assert!(graph().paths_to("win-64", &roots(), "libblas").is_empty());
    assert!(graph().paths_to("linux-64", &roots(), "absent").is_empty());
}

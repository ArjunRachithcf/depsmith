//! Reading inventories from Pixi lockfiles.
use depsmith_core::inventory::pixi_inventory;
#[test]
fn local_editables_with_dynamic_versions_are_retained_as_unknown() {
    let packages = pixi_inventory("version: 7\npackages:\n- pypi: ./\n  name: demo\n- conda: https://conda.anaconda.org/conda-forge/linux-64/lib-blas-1.2.3-h123_0.conda\n").unwrap();
    assert_eq!(packages[0].name, "demo");
    assert_eq!(packages[0].version, "unknown");
    assert_eq!(packages[1].name, "lib-blas");
    assert_eq!(packages[1].version, "1.2.3");
}

#[test]
fn shared_wheels_are_reported_for_each_locked_target_platform() {
    let packages = pixi_inventory("version: 7\nenvironments:\n  default:\n    packages:\n      linux-64:\n      - pypi: https://files.pythonhosted.org/demo.whl\n      win-64:\n      - pypi: https://files.pythonhosted.org/demo.whl\npackages:\n- pypi: https://files.pythonhosted.org/demo.whl\n  name: demo\n  version: '1.0'\n").unwrap();
    assert_eq!(
        packages
            .iter()
            .map(|p| p.platform.as_str())
            .collect::<Vec<_>>(),
        ["linux-64", "win-64"]
    );
}

/// A pixi.lock excerpt like a CUDA workspace's: a noarch conda package shared
/// by two platforms, and a PyPI package whose extras are not installed.
const CUDA_LOCK: &str = r#"version: 6
environments:
  default:
    packages:
      linux-64:
      - conda: https://conda.anaconda.org/conda-forge/noarch/cuda-python-12.9.0-pyhd4762b7_2.conda
      - conda: https://conda.anaconda.org/conda-forge/noarch/cuda-version-12.9-h4f385c5_3.conda
      - pypi: https://files.pythonhosted.org/packages/nuitka-4.1.tar.gz
      win-64:
      - conda: https://conda.anaconda.org/conda-forge/noarch/cuda-python-12.9.0-pyhd4762b7_2.conda
packages:
- conda: https://conda.anaconda.org/conda-forge/noarch/cuda-python-12.9.0-pyhd4762b7_2.conda
  depends:
  - cuda-bindings >=12.9.0,<12.10.0a0
  - cuda-version >=12.0,<13.0a0
  - python >=3.9
  - __unix
- conda: https://conda.anaconda.org/conda-forge/noarch/cuda-version-12.9-h4f385c5_3.conda
  constrains:
  - cudatoolkit 12.9|12.9.*
- pypi: https://files.pythonhosted.org/packages/nuitka-4.1.tar.gz
  name: nuitka
  version: '4.1'
  requires_dist:
  - ordered-set>=4.1.0
  - zstandard>=0.15 ; extra == 'onefile'
  - PyYAML[libyaml] (>=6.0)
  - pywin32 ; sys_platform == 'win32'
"#;

fn requires(
    graph: &depsmith_core::graph::LockGraph,
    platform: &str,
    name: &str,
) -> Vec<(String, Option<String>)> {
    graph
        .nodes
        .iter()
        .find(|n| n.platform == platform && n.name == name)
        .unwrap_or_else(|| panic!("no {name} on {platform}: {graph:?}"))
        .requires
        .iter()
        .map(|r| (r.name.clone(), r.spec.clone()))
        .collect()
}

fn pair(name: &str, spec: Option<&str>) -> (String, Option<String>) {
    (name.into(), spec.map(Into::into))
}

#[test]
fn pixi_graph_records_conda_depends_with_their_specs_per_platform() {
    let graph = depsmith_core::inventory::pixi_graph(CUDA_LOCK).unwrap();
    let cuda_python = [
        pair("cuda-bindings", Some(">=12.9.0,<12.10.0a0")),
        pair("cuda-version", Some(">=12.0,<13.0a0")),
        pair("python", Some(">=3.9")),
    ];
    // Virtual packages such as __unix are platform requirements, not packages.
    assert_eq!(requires(&graph, "linux-64", "cuda-python"), cuda_python);
    assert_eq!(requires(&graph, "win-64", "cuda-python"), cuda_python);
    assert!(requires(&graph, "linux-64", "cuda-version").is_empty());
}

#[test]
fn pixi_graph_records_pypi_requirements_without_uninstalled_extras() {
    let graph = depsmith_core::inventory::pixi_graph(CUDA_LOCK).unwrap();
    assert_eq!(
        requires(&graph, "linux-64", "nuitka"),
        [
            pair("ordered-set", Some(">=4.1.0")),
            pair("PyYAML", Some(">=6.0")),
            pair("pywin32", None),
        ]
    );
}

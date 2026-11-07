//! Reading inventories and lock graphs from Pixi lockfiles.
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
  - helper @ https://example.org/helper-1.0.tar.gz
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
            // A direct URL is not a version requirement.
            pair("helper", None),
        ]
    );
}

#[test]
fn pixi_packages_record_the_environments_that_lock_them() {
    let lock = "version: 6\nenvironments:\n  default:\n    packages:\n      linux-64:\n      - conda: https://conda.anaconda.org/conda-forge/linux-64/numpy-2.0.0-0.conda\n  lint:\n    packages:\n      linux-64:\n      - conda: https://conda.anaconda.org/conda-forge/linux-64/numpy-1.26.4-0.conda\n      win-64:\n      - conda: https://conda.anaconda.org/conda-forge/linux-64/numpy-2.0.0-0.conda\n  test:\n    packages:\n      linux-64:\n      - conda: https://conda.anaconda.org/conda-forge/linux-64/numpy-2.0.0-0.conda\npackages:\n- conda: https://conda.anaconda.org/conda-forge/linux-64/numpy-2.0.0-0.conda\n  name: numpy\n  version: 2.0.0\n- conda: https://conda.anaconda.org/conda-forge/linux-64/numpy-1.26.4-0.conda\n  name: numpy\n  version: 1.26.4\n";
    let mut packages: Vec<(String, String, Vec<String>)> = pixi_inventory(lock)
        .unwrap()
        .into_iter()
        .map(|p| (p.version, p.platform, p.environments))
        .collect();
    packages.sort();
    let envs = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
    assert_eq!(
        packages,
        [
            ("1.26.4".into(), "linux-64".into(), envs(&["lint"])),
            (
                "2.0.0".into(),
                "linux-64".into(),
                envs(&["default", "test"])
            ),
            ("2.0.0".into(), "win-64".into(), envs(&["lint"])),
        ]
    );
}

/// Lock schema 5 names a record's artifact `url` and its ecosystem `kind`.
#[test]
fn schema_5_records_are_read_by_kind_and_url() {
    let lock = r#"version: 5
environments:
  default:
    packages:
      linux-64:
      - conda: https://conda.anaconda.org/conda-forge/linux-64/zlib-1.3.1-hb9d3cd8_2.conda
      - pypi: https://files.pythonhosted.org/packages/six-1.16.0-py2.py3-none-any.whl
packages:
- kind: conda
  name: zlib
  version: 1.3.1
  build: hb9d3cd8_2
  subdir: linux-64
  url: https://conda.anaconda.org/conda-forge/linux-64/zlib-1.3.1-hb9d3cd8_2.conda
- kind: pypi
  name: six
  version: 1.16.0
  url: https://files.pythonhosted.org/packages/six-1.16.0-py2.py3-none-any.whl
"#;
    let packages: Vec<_> = pixi_inventory(lock)
        .unwrap()
        .into_iter()
        .map(|p| (p.ecosystem, p.name, p.version, p.platform, p.environments))
        .collect();
    assert_eq!(
        packages,
        [
            (
                "conda".into(),
                "zlib".into(),
                "1.3.1".into(),
                "linux-64".into(),
                vec!["default".into()]
            ),
            (
                "pypi".into(),
                "six".into(),
                "1.16.0".into(),
                "linux-64".into(),
                vec!["default".into()]
            ),
        ]
    );
}

/// Pixi builds a source package (pixi-build) from the repository or a Git
/// URL: it is the project itself, with no registry artifact or version, so
/// it is not part of the inventory (its name carries a build hash that
/// changes on every lock).
#[test]
fn source_packages_are_not_part_of_the_inventory() {
    let lock = r#"version: 7
environments:
  default:
    packages:
      linux-64:
      - conda_source: demo-core[07415ef2] @ .
      - conda: https://conda.anaconda.org/conda-forge/noarch/six-1.16.0-pyh6c4a22f_0.conda
packages:
- conda_source: demo-core[07415ef2] @ .
  variants:
    target_platform: noarch
  depends:
  - python
  source:
    path: ''
- conda: https://conda.anaconda.org/conda-forge/noarch/six-1.16.0-pyh6c4a22f_0.conda
"#;
    let names: Vec<_> = pixi_inventory(lock)
        .unwrap()
        .into_iter()
        .map(|p| p.name)
        .collect();
    assert_eq!(names, ["six"]);
}

/// Schema 5 locates a local PyPI project by `path` instead of `url`.
#[test]
fn schema_5_local_pypi_projects_are_read_by_path() {
    let lock = "version: 5\npackages:\n- kind: pypi\n  name: demo\n  version: 0.1.0\n  path: ./demo\n  editable: true\n";
    let packages = pixi_inventory(lock).unwrap();
    assert_eq!(
        (packages[0].name.as_str(), packages[0].version.as_str()),
        ("demo", "0.1.0")
    );
}

/// Schema 6 records a package Pixi builds from source as a conda record
/// with a path: the project itself, like a schema 7 `conda_source` record.
#[test]
fn schema_6_path_conda_records_are_not_part_of_the_inventory() {
    // A local archive is a built package, not a source build.
    let lock = "version: 6\npackages:\n- conda: .\n  name: demo-core\n  version: 0.1.0\n  input:\n    hash: abc\n- conda: https://conda.anaconda.org/conda-forge/noarch/six-1.16.0-pyh6c4a22f_0.conda\n- conda: ./channel/noarch/vendored-1.0-0.conda\n";
    let names: Vec<_> = pixi_inventory(lock)
        .unwrap()
        .into_iter()
        .map(|p| p.name)
        .collect();
    assert_eq!(names, ["six", "vendored"]);
}

/// A source package stays in the lock graph, so what only it requires
/// still has a path from the manifest.
#[test]
fn source_packages_keep_their_requirements_in_the_graph() {
    let lock = r#"version: 7
environments:
  default:
    packages:
      linux-64:
      - conda_source: demo-core[07415ef2] @ .
      - conda: https://conda.anaconda.org/conda-forge/linux-64/zlib-1.3.1-hb9d3cd8_2.conda
packages:
- conda_source: demo-core[07415ef2] @ .
  depends:
  - zlib >=1.3
- conda: https://conda.anaconda.org/conda-forge/linux-64/zlib-1.3.1-hb9d3cd8_2.conda
"#;
    let graph = depsmith_core::inventory::pixi_graph(lock).unwrap();
    assert_eq!(
        requires(&graph, "linux-64", "demo-core"),
        [pair("zlib", Some(">=1.3"))]
    );
}

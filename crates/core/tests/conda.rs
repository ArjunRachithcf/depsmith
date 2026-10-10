//! The conda adapter offline: `environment.yml` targets, conda and `pip:`
//! declarations, the conda-lock inventory, and suggestions and `--accept`
//! with a fixture registry and a stand-in conda-lock.
use depsmith_core::{
    adapter::Adapter,
    conda::{lock_inventory, Conda},
    conformance, Engine, Target,
};
use std::{collections::BTreeMap, fs};

const ENVIRONMENT: &str = "name: demo
channels:
  - conda-forge
dependencies:
  - python=3.12
  - conda-forge::six ==1.16.0  # pinned
  - numpy >=1.26,<2
  - pip
  - pip:
    - idna==3.6
    - Requests[socks] >=2  # floor
platforms:
  - linux-64
  - win-64
";

fn target() -> Target {
    Target {
        id: "conda:environment.yml".into(),
        manager: "conda".into(),
        manifest: "environment.yml".into(),
    }
}

#[test]
fn environment_files_are_targets_with_conda_and_pip_declarations() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("environment.yml"), ENVIRONMENT).unwrap();
    fs::create_dir_all(root.path().join(".github/workflows")).unwrap();
    fs::write(
        root.path().join(".github/workflows/ci.yml"),
        "on: push\njobs: {}\n",
    )
    .unwrap();
    let ids: Vec<String> = Engine::default()
        .discover(root.path())
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect();
    assert!(ids.contains(&"conda:environment.yml".into()), "{ids:?}");
    assert!(
        !ids.iter().any(|id| id.starts_with("conda:.github")),
        "{ids:?}"
    );
    let declared: Vec<(String, String, String, String)> = Conda
        .declarations(root.path(), &target())
        .unwrap()
        .into_iter()
        .map(|d| (d.ecosystem, d.location, d.package, d.requirement))
        .collect();
    let expected = [
        ("conda", "dependencies[0]", "python", "=3.12"),
        ("conda", "dependencies[1]", "six", "==1.16.0"),
        ("conda", "dependencies[2]", "numpy", ">=1.26,<2"),
        ("conda", "dependencies[3]", "pip", ""),
        ("pypi", "dependencies[4].pip[0]", "idna", "==3.6"),
        ("pypi", "dependencies[4].pip[1]", "Requests", ">=2"),
    ]
    .map(|(e, l, p, r)| (e.into(), l.into(), p.into(), r.into()));
    assert_eq!(declared, expected);
    let selected = Conda
        .select(
            root.path(),
            &target(),
            &["SIX".into(), "requests".into(), "nope".into()],
        )
        .unwrap();
    assert_eq!(
        selected,
        BTreeMap::from([
            ("SIX".into(), "six".into()),
            ("requests".into(), "Requests".into())
        ])
    );
}

#[test]
fn lock_inventory_reads_conda_and_pip_packages_per_platform() {
    let lock = "version: 1\nmetadata:\n  platforms: [linux-64]\n  sources: [environment.yml]\npackage:\n- name: six\n  version: 1.16.0\n  manager: conda\n  platform: linux-64\n  url: https://conda.anaconda.org/conda-forge/noarch/six-1.16.0-pyhd8ed1ab_1.conda\n  hash: {md5: a, sha256: b}\n- name: idna\n  version: '3.6'\n  manager: pip\n  platform: linux-64\n  url: https://files.pythonhosted.org/packages/c2/e7/x/idna-3.6-py3-none-any.whl\n  hash: {sha256: c}\n";
    let packages: Vec<_> = lock_inventory(lock)
        .unwrap()
        .into_iter()
        .map(|p| (p.ecosystem, p.name, p.version, p.platform))
        .collect();
    assert_eq!(
        packages,
        [
            (
                "conda".into(),
                "six".into(),
                "1.16.0".into(),
                "linux-64".into()
            ),
            (
                "pypi".into(),
                "idna".into(),
                "3.6".into(),
                "linux-64".into()
            )
        ]
    );
}

/// A conda-lock lock where the declared numpy needs libblas, which needs
/// libopenblas, and the pip package requests needs urllib3; linux-64 only
/// for libopenblas, which win-64 does without.
const GRAPH_LOCK: &str = "version: 1
metadata:
  platforms: [linux-64, win-64]
  sources: [environment.yml]
package:
- name: numpy
  version: 1.26.4
  manager: conda
  platform: linux-64
  dependencies:
    libblas: '>=3.9.0,<4.0a0'
    python: '>=3.12,<3.13.0a0'
    __glibc: '>=2.17'
  url: https://conda.anaconda.org/conda-forge/linux-64/numpy-1.26.4-py312h.conda
  hash: {md5: a, sha256: b}
- name: libblas
  version: 3.9.0
  manager: conda
  platform: linux-64
  dependencies:
    libopenblas: ''
  url: https://conda.anaconda.org/conda-forge/linux-64/libblas-3.9.0-20_linux64_openblas.conda
  hash: {md5: a, sha256: b}
- name: libopenblas
  version: 0.3.25
  manager: conda
  platform: linux-64
  dependencies: {}
  url: https://conda.anaconda.org/conda-forge/linux-64/libopenblas-0.3.25-pthreads.conda
  hash: {md5: a, sha256: b}
- name: numpy
  version: 1.26.4
  manager: conda
  platform: win-64
  dependencies:
    libblas: '>=3.9.0,<4.0a0'
  url: https://conda.anaconda.org/conda-forge/win-64/numpy-1.26.4-py312h.conda
  hash: {md5: a, sha256: b}
- name: libblas
  version: 3.9.0
  manager: conda
  platform: win-64
  url: https://conda.anaconda.org/conda-forge/win-64/libblas-3.9.0-mkl.conda
  hash: {md5: a, sha256: b}
- name: requests
  version: 2.31.0
  manager: pip
  platform: linux-64
  dependencies:
    urllib3: <3,>=1.21.1
  url: https://files.pythonhosted.org/packages/x/requests-2.31.0-py3-none-any.whl
  hash: {sha256: c}
- name: urllib3
  version: 2.2.1
  manager: pip
  platform: linux-64
  dependencies: {}
  url: https://files.pythonhosted.org/packages/x/urllib3-2.2.1-py3-none-any.whl
  hash: {sha256: c}
";

#[test]
fn lock_graph_is_none_without_a_lock() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("environment.yml"), ENVIRONMENT).unwrap();
    assert!(Conda.lock_graph(root.path(), &target()).unwrap().is_none());
}

#[test]
fn lock_graph_reads_dependencies_with_requirements_per_platform() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("environment.yml"), ENVIRONMENT).unwrap();
    fs::write(root.path().join("conda-lock.yml"), GRAPH_LOCK).unwrap();
    let graph = Conda.lock_graph(root.path(), &target()).unwrap().unwrap();
    let nodes: Vec<_> = graph
        .nodes
        .iter()
        .map(|n| {
            (
                n.ecosystem.as_str(),
                n.name.as_str(),
                n.platform.as_str(),
                n.requires
                    .iter()
                    .map(|r| (r.name.as_str(), r.spec.as_deref()))
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    // Virtual packages such as __glibc are not edges, and an empty
    // requirement is none.
    assert_eq!(
        nodes,
        [
            (
                "conda",
                "numpy",
                "linux-64",
                vec![
                    ("libblas", Some(">=3.9.0,<4.0a0")),
                    ("python", Some(">=3.12,<3.13.0a0"))
                ]
            ),
            ("conda", "libblas", "linux-64", vec![("libopenblas", None)]),
            ("conda", "libopenblas", "linux-64", vec![]),
            (
                "conda",
                "numpy",
                "win-64",
                vec![("libblas", Some(">=3.9.0,<4.0a0"))]
            ),
            ("conda", "libblas", "win-64", vec![]),
            (
                "pypi",
                "requests",
                "linux-64",
                vec![("urllib3", Some("<3,>=1.21.1"))]
            ),
            ("pypi", "urllib3", "linux-64", vec![]),
        ]
    );
}

#[test]
fn lock_graph_paths_lead_from_declared_to_transitive_packages() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("environment.yml"), ENVIRONMENT).unwrap();
    fs::write(root.path().join("environment.conda-lock.yml"), GRAPH_LOCK).unwrap();
    let graph = Conda.lock_graph(root.path(), &target()).unwrap().unwrap();
    let roots = [
        ("conda".to_owned(), "numpy".to_owned()),
        ("pypi".to_owned(), "Requests".to_owned()),
    ];
    assert_eq!(
        graph.paths_to("linux-64", &roots, "conda", "libopenblas"),
        [vec!["numpy", "libblas", "libopenblas"]]
    );
    assert_eq!(
        graph.paths_to("linux-64", &roots, "pypi", "urllib3"),
        [vec!["Requests", "urllib3"]]
    );
    assert!(graph
        .paths_to("win-64", &roots, "conda", "libopenblas")
        .is_empty());
}

#[test]
fn conda_passes_the_conformance_suite() {
    let fixture = conformance::Fixture {
        files: vec![("environment.yml".into(), ENVIRONMENT.into())],
        target: "conda:environment.yml".into(),
        declared: vec![("SIX".into(), "six".into())],
    };
    conformance::check(&|| Box::new(Conda) as Box<dyn Adapter>, &fixture).unwrap();
}

#[cfg(unix)]
mod stand_in {
    use super::*;
    use depsmith_core::{
        adapter::AdapterSpec,
        constraints::{FixtureRelease, RegistryConfig},
        Proposal, Result, UpdateOptions,
    };
    use std::{os::unix::fs::PermissionsExt, path::Path};

    const LOCK: &str = "version: 1\nmetadata:\n  platforms: [linux-64]\n  sources: [environment.yml]\npackage:\n- name: six\n  version: 1.16.0\n  manager: conda\n  platform: linux-64\n  url: https://conda.anaconda.org/conda-forge/noarch/six-1.16.0-pyhd8ed1ab_1.conda\n  hash: {md5: a, sha256: b}\n";

    /// A conda-lock that writes a fixed lock where `--lockfile` points.
    fn conda_lock(dir: &Path) -> String {
        let script = dir.join("conda-lock");
        fs::write(dir.join("lock"), LOCK).unwrap();
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nwhile [ $# -gt 0 ]; do [ \"$1\" = --lockfile ] && cp '{}' \"$2\"; shift; done\n",
                dir.join("lock").display()
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        script.to_string_lossy().into_owned()
    }

    /// The conda adapter with an inline registry for six.
    struct Fixed;
    impl Adapter for Fixed {
        fn spec(&self) -> AdapterSpec {
            Conda.spec()
        }
        fn detects(&self, path: &Path, content: &str) -> bool {
            Conda.detects(path, content)
        }
        fn declarations(
            &self,
            root: &Path,
            target: &Target,
        ) -> Result<Vec<depsmith_core::constraints::Declaration>> {
            Conda.declarations(root, target)
        }
        fn rewrite(
            &self,
            stage: &Path,
            target: &Target,
            edits: &[depsmith_core::constraints::Edit],
        ) -> Result<()> {
            Conda.rewrite(stage, target, edits)
        }
        fn select(
            &self,
            root: &Path,
            target: &Target,
            requested: &[String],
        ) -> Result<BTreeMap<String, String>> {
            Conda.select(root, target, requested)
        }
        fn availability(
            &self,
            _: &Path,
            _: &Target,
        ) -> Result<depsmith_core::constraints::AvailabilityConfig> {
            let release = |version: &str| FixtureRelease {
                version: version.into(),
                url: format!("https://conda.anaconda.org/conda-forge/noarch/six-{version}.conda"),
                sha256: format!("sum-{version}"),
            };
            Ok(depsmith_core::constraints::AvailabilityConfig {
                registries: BTreeMap::from([(
                    "conda".into(),
                    RegistryConfig::Fixture {
                        releases: BTreeMap::from([(
                            "six".into(),
                            vec![release("1.16.0"), release("1.17.0")],
                        )]),
                        failing: vec![],
                    },
                )]),
                exclude_newer: None,
            })
        }
        fn prepare(
            &self,
            stage: &Path,
            target: &Target,
            options: &UpdateOptions,
        ) -> Result<depsmith_core::adapter::Candidate> {
            Conda.prepare(stage, target, options)
        }
    }

    fn prepare(environment: &str, accept: &[&str]) -> Proposal {
        let root = tempfile::tempdir().unwrap();
        let tools = tempfile::tempdir().unwrap();
        fs::write(root.path().join("environment.yml"), environment).unwrap();
        let options = UpdateOptions {
            tools: BTreeMap::from([
                ("conda-lock".into(), conda_lock(tools.path())),
                ("conda".into(), "micromamba".into()),
            ]),
            accept: accept.iter().map(|a| a.to_string()).collect(),
            ..Default::default()
        };
        Engine::new(vec![Box::new(Fixed)])
            .prepare(root.path(), &["conda:environment.yml".into()], options)
            .unwrap()
    }

    const SIX: &str =
        "channels: [conda-forge]\ndependencies:\n  - conda-forge::six ==1.16.0  # pinned\n";

    #[test]
    fn pins_are_suggested_and_accepted_in_place() {
        let proposal = prepare(SIX, &[]);
        assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
        let six = proposal
            .suggestions
            .iter()
            .find(|s| s.package == "six")
            .unwrap();
        assert!(
            six.evidence[0].contains("1.17.0 is excluded (newest allowed 1.16.0)"),
            "{:?}",
            six.evidence
        );
        assert!(proposal.dependencies.iter().any(|d| d
            .after
            .as_ref()
            .is_some_and(|p| p.name == "six" && p.ecosystem == "conda")));

        let proposal = prepare(SIX, &["six"]);
        assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
        let environment = proposal
            .changes
            .iter()
            .find(|c| c.path == Path::new("environment.yml"))
            .unwrap();
        assert_eq!(
            environment.after,
            SIX.replace("==1.16.0", "==1.17.0"),
            "comments and layout are kept"
        );
    }

    #[test]
    fn a_lock_missing_a_declared_package_is_rejected() {
        let proposal = prepare(
            "channels: [conda-forge]\ndependencies:\n  - six\n  - numpy\n",
            &[],
        );
        assert_eq!(proposal.failures.len(), 1, "{:?}", proposal.failures);
        assert!(
            proposal.failures[0]
                .message
                .contains("numpy is not locked for linux-64"),
            "{}",
            proposal.failures[0].message
        );
    }
}

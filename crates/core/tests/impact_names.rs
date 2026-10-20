//! Import names: the modules each distribution provides, read from the
//! target's installed environments, else a built-in table, then for a
//! namespace distribution the module below its namespace, else its
//! normalized name.
use depsmith_core::{impact::ImportNames, Target};
use std::{fs, path::Path};

fn write(root: &Path, files: &[(&str, &str)]) {
    for (path, text) in files {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
}

fn target(manifest: &str) -> Target {
    Target {
        id: format!("uv:{manifest}"),
        manager: "uv".into(),
        manifest: manifest.into(),
    }
}

const SITE: &str = "svc/.venv/lib/python3.12/site-packages";

#[test]
fn a_virtual_environment_records_the_modules_of_each_distribution() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let record = |dist: &str| format!("{SITE}/{dist}/RECORD");
    write(
        root,
        &[
            ("svc/pyproject.toml", "[project]\nname = \"svc\"\n"),
            (
                &record("PyYAML-6.0.2.dist-info"),
                "yaml/__init__.py,sha256=x,1\n\
                 yaml/composer.py,sha256=x,1\n\
                 _yaml/__init__.py,sha256=x,1\n\
                 PyYAML-6.0.2.dist-info/RECORD,,\n\
                 yaml/__pycache__/composer.cpython-312.pyc,,\n",
            ),
            (
                &record("protobuf-5.28.0.dist-info"),
                "google/protobuf/__init__.py,sha256=x,1\n\
                 google/_upb/_message.abi3.so,sha256=x,1\n\
                 protobuf-5.28.0.dist-info/METADATA,,\n",
            ),
            (
                &record("six-1.16.0.dist-info"),
                "six.py,sha256=x,1\n../../../bin/six,sha256=x,1\n",
            ),
            (
                &record("cffi-1.17.0.dist-info"),
                "_cffi_backend.cpython-312-x86_64-linux-gnu.so,sha256=x,1\n\
                 cffi/__init__.py,sha256=x,1\n\
                 \"cffi/odd,name.py\",sha256=x,1\n",
            ),
            (
                &format!("{SITE}/attrs-24.2.0.dist-info/top_level.txt"),
                "attr\nattrs\n",
            ),
        ],
    );
    let names = ImportNames::read(root, &target("svc/pyproject.toml"));
    assert_eq!(names.modules("pyyaml"), ["_yaml", "yaml"]);
    assert_eq!(
        names.modules("PyYAML"),
        ["_yaml", "yaml"],
        "names normalize"
    );
    assert_eq!(
        names.modules("protobuf"),
        ["google._upb._message", "google.protobuf"],
        "below namespace packages, down to a regular package or a module"
    );
    assert_eq!(names.modules("six"), ["six"]);
    assert_eq!(names.modules("cffi"), ["_cffi_backend", "cffi"]);
    assert_eq!(
        names.modules("attrs"),
        ["attr", "attrs"],
        "top_level.txt without a RECORD"
    );
    assert!(names.warnings().is_empty(), "{:?}", names.warnings());
}

#[test]
fn pixi_environments_add_their_conda_packages() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        &[
            ("svc/pixi.toml", "[workspace]\nname = \"svc\"\n"),
            (
                "svc/.pixi/envs/default/conda-meta/py-opencv-4.10.0-h1.json",
                r#"{"name": "py-opencv", "version": "4.10.0",
                    "files": ["lib/python3.12/site-packages/cv2/__init__.py",
                              "lib/python3.12/site-packages/cv2/load_config_py3.py",
                              "share/opencv4/haarcascade.xml"]}"#,
            ),
            (
                "svc/.pixi/envs/lint/conda-meta/pytorch-2.4.0-h2.json",
                r#"{"name": "pytorch",
                    "files": ["Lib/site-packages/torch/__init__.py",
                              "Lib/site-packages/functorch/__init__.py"]}"#,
            ),
            (
                "svc/.pixi/envs/default/conda-meta/openssl-3.3.0-h3.json",
                r#"{"name": "openssl", "files": ["lib/libssl.so.3"]}"#,
            ),
            (
                "svc/.pixi/envs/default/lib/python3.12/site-packages/idna-3.8.dist-info/RECORD",
                "idna/__init__.py,sha256=x,1\n",
            ),
        ],
    );
    let names = ImportNames::read(
        root,
        &Target {
            id: "pixi:svc/pixi.toml".into(),
            manager: "pixi".into(),
            manifest: "svc/pixi.toml".into(),
        },
    );
    assert_eq!(names.modules("py-opencv"), ["cv2"]);
    assert_eq!(
        names.modules("pytorch"),
        ["functorch", "torch"],
        "every environment, Windows layout too"
    );
    assert_eq!(names.modules("idna"), ["idna"], "PyPI packages in pixi");
    assert_eq!(
        names.modules("openssl"),
        ["openssl"],
        "no Python modules: the normalized name"
    );
}

#[test]
fn a_workspace_member_reads_the_environment_of_the_workspace_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        &[
            ("svc/pyproject.toml", "[project]\nname = \"svc\"\n"),
            (
                "svc/libs/shared/pyproject.toml",
                "[project]\nname = \"shared\"\n",
            ),
            (
                &format!("{SITE}/PyYAML-6.0.2.dist-info/top_level.txt"),
                "yaml\n",
            ),
        ],
    );
    let names = ImportNames::read(root, &target("svc/libs/shared/pyproject.toml"));
    assert_eq!(names.modules("pyyaml"), ["yaml"]);
}

#[test]
fn without_an_environment_the_table_then_the_normalized_name_answer_with_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        &[("svc/pyproject.toml", "[project]\nname = \"svc\"\n")],
    );
    let names = ImportNames::read(root, &target("svc/pyproject.toml"));
    assert_eq!(names.modules("Pillow"), ["PIL"]);
    assert_eq!(names.modules("scikit-learn"), ["sklearn"]);
    assert_eq!(names.modules("protobuf"), ["google.protobuf"]);
    assert_eq!(names.modules("typing-extensions"), ["typing_extensions"]);
    assert_eq!(names.warnings().len(), 1, "{:?}", names.warnings());
    assert!(
        names.warnings()[0].contains("svc"),
        "{:?}",
        names.warnings()
    );
    assert!(ImportNames::default().warnings().is_empty());
}

#[test]
fn stub_packages_name_the_modules_they_type() {
    let names = ImportNames::default();
    assert_eq!(names.modules("types-PyYAML"), ["yaml"]);
    assert_eq!(names.modules("types-requests"), ["requests"]);
    assert_eq!(names.modules("pandas-stubs"), ["pandas"]);
}

#[test]
fn only_importable_modules_count_down_to_the_first_regular_package() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let record = |dist: &str| format!("{SITE}/{dist}/RECORD");
    write(
        root,
        &[
            ("svc/pyproject.toml", "[project]\nname = \"svc\"\n"),
            (
                &record("google_cloud_storage-2.18.0.dist-info"),
                "google/cloud/storage/__init__.py,,\n\
                 google/cloud/storage/blob.py,,\n",
            ),
            (
                &record("numpy-2.1.0.dist-info"),
                "numpy/__init__.py,,\n\
                 numpy.libs/libopenblas.so,,\n\
                 tests/test_numpy.py,,\n\
                 docs/conf.py,,\n",
            ),
            (
                // A setuptools editable install: only a finder and a .pth.
                &record("svc_tools-0.1.0.dist-info"),
                "__editable___svc_tools_0_1_0_finder.py,,\n\
                 __editable__.svc_tools-0.1.0.pth,,\n",
            ),
            (&record("svc_cli-0.1.0.dist-info"), "_svc_cli.pth,,\n"),
            (
                &format!("{SITE}/legacy-name-1.0.dist-info/top_level.txt"),
                "legacy_name\n",
            ),
            (
                &format!("{SITE}/oldpkg-1.0-py3.12.egg-info/top_level.txt"),
                "oldpkg\n",
            ),
        ],
    );
    let names = ImportNames::read(root, &target("svc/pyproject.toml"));
    assert_eq!(
        names.modules("google-cloud-storage"),
        ["google.cloud.storage"],
        "nested namespace packages"
    );
    assert_eq!(names.modules("numpy"), ["numpy"]);
    assert_eq!(
        names.modules("svc-tools"),
        ["svc_tools"],
        "an editable install falls back"
    );
    assert_eq!(
        names.modules("svc-cli"),
        ["svc_cli"],
        "a .pth alone falls back"
    );
    assert_eq!(names.modules("legacy-name"), ["legacy_name"]);
    assert_eq!(names.modules("oldpkg"), ["oldpkg"]);
    assert_eq!(
        names.modules("absent"),
        ["absent"],
        "not installed: the normalized name"
    );
}

#[test]
fn noarch_conda_packages_and_unreadable_records_are_read_or_warned() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        &[
            ("svc/pixi.toml", "[workspace]\nname = \"svc\"\n"),
            (
                "svc/.pixi/envs/default/conda-meta/requests-2.32.3-pyhd8ed1ab_0.json",
                r#"{"name": "requests", "noarch": {"type": "python"},
                    "files": ["site-packages/requests/__init__.py"]}"#,
            ),
            (
                "svc/.pixi/envs/default/conda-meta/broken-1.0-h0.json",
                "{ not json",
            ),
        ],
    );
    let names = ImportNames::read(
        root,
        &Target {
            id: "pixi:svc/pixi.toml".into(),
            manager: "pixi".into(),
            manifest: "svc/pixi.toml".into(),
        },
    );
    assert_eq!(names.modules("requests"), ["requests"]);
    assert_eq!(names.warnings().len(), 1, "{:?}", names.warnings());
    assert!(
        names.warnings()[0].contains("broken-1.0-h0.json"),
        "{:?}",
        names.warnings()
    );
}

#[test]
fn namespace_distributions_name_the_module_below_their_namespace() {
    let names = ImportNames::default();
    assert_eq!(names.modules("opentelemetry-sdk"), ["opentelemetry.sdk"]);
    assert!(
        names
            .modules("opentelemetry-api")
            .contains(&"opentelemetry.trace".to_owned()),
        "the API is the namespace's own modules, not the namespace"
    );
    assert_eq!(names.modules("azure-storage-blob"), ["azure.storage.blob"]);
    assert_eq!(
        names.modules("google-cloud-pubsub"),
        ["google.cloud.pubsub"]
    );
    for (package, module) in [
        (
            "opentelemetry-exporter-otlp-proto-http",
            "opentelemetry.exporter.otlp.proto.http",
        ),
        ("azure-storage-file-datalake", "azure.storage.filedatalake"),
        (
            "google-cloud-bigquery-storage",
            "google.cloud.bigquery_storage",
        ),
        (
            "backports-functools-lru-cache",
            "backports.functools_lru_cache",
        ),
    ] {
        assert!(
            names.modules(package).contains(&module.to_owned()),
            "{package}: {:?}",
            names.modules(package)
        );
    }
    assert_eq!(names.modules("hydra-core"), ["hydra"]);
    assert_eq!(names.modules("ansible-core"), ["ansible"]);
}

#[test]
fn a_target_without_python_has_no_environment_to_miss() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), &[("web/package.json", "{}")]);
    let names = ImportNames::read(
        dir.path(),
        &Target {
            id: "npm:web/package.json".into(),
            manager: "npm".into(),
            manifest: "web/package.json".into(),
        },
    );
    assert!(names.warnings().is_empty(), "{:?}", names.warnings());
}

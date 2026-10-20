//! Usage sites of the affected packages in first-party code, and the files
//! that reach them through the first-party import graph.
use depsmith_core::{
    impact::{usages, Affected, AffectedFile, AffectedPackage, Hop, ImportNames, UsageSite},
    Target,
};
use std::{fs, path::Path};

fn write(root: &Path, files: &[(&str, &str)]) {
    for (path, text) in files {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
}

fn package(ecosystem: &str, name: &str, stdlib: &[&str]) -> AffectedPackage {
    AffectedPackage {
        ecosystem: ecosystem.into(),
        name: name.into(),
        platform: "linux-64".into(),
        declared: false,
        changed: true,
        reaches: vec![],
        stdlib: stdlib.iter().map(|m| (*m).into()).collect(),
    }
}

fn affected(packages: Vec<AffectedPackage>) -> Affected {
    Affected {
        packages,
        ..Affected::default()
    }
}

fn target() -> Target {
    Target {
        id: "uv:svc/pyproject.toml".into(),
        manager: "uv".into(),
        manifest: "svc/pyproject.toml".into(),
    }
}

fn site(file: &str, line: usize, module: &str, package: &str) -> UsageSite {
    UsageSite {
        file: file.into(),
        line,
        module: module.into(),
        package: package.into(),
    }
}

fn hop(file: &str, line: usize) -> Hop {
    Hop {
        file: file.into(),
        line,
    }
}

fn file<'a>(files: &'a [AffectedFile], path: &str) -> Option<&'a AffectedFile> {
    files.iter().find(|f| f.file == Path::new(path))
}

/// A uv workspace in `svc/` with a member in `svc/libs/shared/`.
fn workspace(root: &Path) {
    write(
        root,
        &[
            (
                "svc/pyproject.toml",
                "[project]\nname = \"svc\"\n[tool.uv.workspace]\nmembers = [\"libs/*\"]\n",
            ),
            ("svc/src/svc/__init__.py", ""),
            ("svc/src/svc/codec.py", "import os\nimport idna.core\n"),
            ("svc/src/svc/api.py", "\nfrom . import codec\n"),
            ("svc/src/svc/worker.py", "from shared import net\n"),
            ("svc/src/svc/unrelated.py", "import yaml\n"),
            ("svc/tests/test_api.py", "from svc.api import handler\n"),
            (
                "svc/libs/shared/pyproject.toml",
                "[project]\nname = \"shared\"\n",
            ),
            ("svc/libs/shared/src/shared/__init__.py", ""),
            ("svc/libs/shared/src/shared/net.py", "import ssl\n"),
            ("svc/.venv/lib/site-packages/vendored.py", "import idna\n"),
            ("svc/build/lib/svc/codec.py", "import idna\n"),
            ("web/main.py", "import idna\n"),
        ],
    );
}

#[test]
fn usage_sites_are_the_imports_of_affected_packages_in_the_target_directory() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    workspace(root);
    let result = usages(
        root,
        &target(),
        &affected(vec![
            package("pypi", "idna", &[]),
            package("conda", "openssl", &["ssl", "hashlib"]),
        ]),
        &ImportNames::default(),
    )
    .unwrap();
    assert_eq!(
        result.sites,
        [
            site("svc/libs/shared/src/shared/net.py", 1, "ssl", "openssl"),
            site("svc/src/svc/codec.py", 2, "idna.core", "idna"),
        ],
        "first-party code only, not .venv, build output or another target"
    );
}

#[test]
fn affected_files_reach_a_usage_site_through_first_party_imports() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    workspace(root);
    let result = usages(
        root,
        &target(),
        &affected(vec![
            package("pypi", "idna", &[]),
            package("conda", "openssl", &["ssl"]),
        ]),
        &ImportNames::default(),
    )
    .unwrap();
    let test = file(&result.files, "svc/tests/test_api.py").expect("the test reaches idna");
    assert_eq!(
        test.chain,
        [
            hop("svc/tests/test_api.py", 1),
            hop("svc/src/svc/api.py", 2),
            hop("svc/src/svc/codec.py", 2),
        ]
    );
    assert_eq!(test.packages, ["idna"]);
    let worker = file(&result.files, "svc/src/svc/worker.py").expect("through a member");
    assert_eq!(worker.packages, ["openssl"]);
    assert_eq!(
        file(&result.files, "svc/src/svc/codec.py").unwrap().chain,
        [hop("svc/src/svc/codec.py", 2)],
        "a usage site is its own chain"
    );
    assert!(file(&result.files, "svc/src/svc/unrelated.py").is_none());
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
}

#[test]
fn a_first_party_module_shadows_a_package_of_the_same_name() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        &[
            ("svc/pyproject.toml", "[project]\nname = \"svc\"\n"),
            ("svc/idna.py", "\n"),
            ("svc/main.py", "import idna\n"),
        ],
    );
    let result = usages(
        root,
        &target(),
        &affected(vec![package("pypi", "idna", &[])]),
        &ImportNames::default(),
    )
    .unwrap();
    assert!(result.sites.is_empty(), "{:?}", result.sites);
}

#[test]
fn distribution_names_match_their_normalized_import_name() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        &[
            ("svc/pyproject.toml", "[project]\nname = \"svc\"\n"),
            ("svc/main.py", "from typing_extensions import Self\n"),
        ],
    );
    let result = usages(
        root,
        &target(),
        &affected(vec![package("pypi", "Typing.Extensions", &[])]),
        &ImportNames::default(),
    )
    .unwrap();
    assert_eq!(
        result.sites,
        [site(
            "svc/main.py",
            1,
            "typing_extensions",
            "Typing.Extensions"
        )]
    );
}

#[test]
fn unparsed_syntax_is_a_warning_and_the_imports_around_it_still_count() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        &[
            ("svc/pyproject.toml", "[project]\nname = \"svc\"\n"),
            ("svc/main.py", "import idna\n\ndef broken(:\n    pass\n"),
        ],
    );
    let result = usages(
        root,
        &target(),
        &affected(vec![package("pypi", "idna", &[])]),
        &ImportNames::default(),
    )
    .unwrap();
    assert_eq!(result.sites, [site("svc/main.py", 1, "idna", "idna")]);
    assert_eq!(result.warnings.len(), 1, "{:?}", result.warnings);
    assert!(
        result.warnings[0].contains("svc/main.py:3"),
        "{:?}",
        result.warnings
    );
}

#[test]
fn importing_a_submodule_runs_the_package_init_files_above_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        &[
            ("svc/pyproject.toml", "[project]\nname = \"svc\"\n"),
            ("svc/src/svc/__init__.py", "import idna\n"),
            ("svc/src/svc/api/__init__.py", ""),
            ("svc/src/svc/api/routes.py", "\n"),
            ("svc/main.py", "from svc.api import routes\n"),
        ],
    );
    let result = usages(
        root,
        &target(),
        &affected(vec![package("pypi", "idna", &[])]),
        &ImportNames::default(),
    )
    .unwrap();
    let main = file(&result.files, "svc/main.py").expect("svc/__init__.py runs first");
    assert_eq!(
        main.chain,
        [hop("svc/main.py", 1), hop("svc/src/svc/__init__.py", 1)]
    );
}

#[test]
fn a_namespace_package_is_first_party() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        &[
            ("svc/pyproject.toml", "[project]\nname = \"svc\"\n"),
            ("svc/plugins/loader.py", "\n"),
            ("svc/main.py", "import plugins\n"),
        ],
    );
    let result = usages(
        root,
        &target(),
        &affected(vec![package("pypi", "plugins", &[])]),
        &ImportNames::default(),
    )
    .unwrap();
    assert!(result.sites.is_empty(), "{:?}", result.sites);
}

#[test]
fn an_installed_distribution_matches_the_modules_it_provides() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        &[
            ("svc/pyproject.toml", "[project]\nname = \"svc\"\n"),
            ("svc/main.py", "import yaml\nimport pyyaml_helpers\n"),
        ],
    );
    let mut names = ImportNames::default();
    names.insert("PyYAML", ["yaml".to_owned()]);
    let result = usages(
        root,
        &target(),
        &affected(vec![package("pypi", "pyyaml", &[])]),
        &names,
    )
    .unwrap();
    assert_eq!(result.sites, [site("svc/main.py", 1, "yaml", "pyyaml")]);
}

#[test]
fn a_namespace_package_matches_only_imports_below_its_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        &[
            ("svc/pyproject.toml", "[project]\nname = \"svc\"\n"),
            (
                "svc/main.py",
                "from google.protobuf import message\n\
                 from google import protobuf\n\
                 import google.cloud.storage\n\
                 import google\n\
                 import google.protobuf_extras\n",
            ),
        ],
    );
    let result = usages(
        root,
        &target(),
        &affected(vec![package("pypi", "protobuf", &[])]),
        &ImportNames::default(),
    )
    .unwrap();
    assert_eq!(
        result.sites,
        [
            site("svc/main.py", 1, "google.protobuf", "protobuf"),
            site("svc/main.py", 2, "google.protobuf", "protobuf"),
        ]
    );
}

#[test]
fn a_missing_environment_is_a_warning_of_the_usages() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        &[("svc/pyproject.toml", "[project]\nname = \"svc\"\n")],
    );
    let names = ImportNames::read(root, &target());
    let result = usages(root, &target(), &affected(vec![]), &names).unwrap();
    assert_eq!(result.warnings, names.warnings());
}

//! Usage sites of the affected packages in first-party code, and the files
//! that reach them through the first-party import graph.
use depsmith_core::{
    impact::{
        usages, Affected, AffectedFile, AffectedPackage, ByNameKind, ByNameReference, Hop,
        ImportNames, UsageSite,
    },
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

fn npm_target() -> Target {
    Target {
        id: "npm:web/package.json".into(),
        manager: "npm".into(),
        manifest: "web/package.json".into(),
    }
}

fn js_usages(root: &Path, packages: Vec<AffectedPackage>) -> depsmith_core::impact::Usages {
    usages(
        root,
        &npm_target(),
        &affected(packages),
        &ImportNames::default(),
    )
    .unwrap()
}

#[test]
fn js_relative_specifiers_resolve_to_source_files_and_bare_ones_to_packages() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        &[
            ("web/package.json", r#"{"name": "web"}"#),
            ("web/src/date.ts", "import { format } from 'date-fns/format';\n"),
            ("web/src/ui/index.tsx", "export * from '../date.js';\n"),
            ("web/src/app.ts", "import { Card } from './ui';\n"),
            (
                "web/src/shims.ts",
                "import fs from 'node:fs';\nimport { Buffer } from 'buffer';\nimport b from 'node:buffer';\nimport { EventEmitter } from 'events';\n",
            ),
            ("web/src/main.mjs", "import './app.js';\nimport data from './data.json';\n"),
            ("web/src/data.json", "{}"),
            (
                "web/src/http.ts",
                "/// <reference types=\"node\" />\nimport { get } from '@scope/http/client';\n",
            ),
            ("web/test/app.test.ts", "vi.mock('../src/app');\nimport { vi } from 'vitest';\n"),
            ("web/test/date.test.js", "jest.mock('../src/date');\n"),
            ("web/src/Widget.vue", "<script>import x from 'date-fns'</script>\n"),
            ("web/node_modules/date-fns/index.js", "import './x';\n"),
            ("web/coverage/lcov-report/prettify.js", "require('date-fns');\n"),
        ],
    );
    let found = js_usages(
        root,
        vec![
            package("npm", "date-fns", &[]),
            package("npm", "@types/node", &[]),
            package("npm", "@scope/http", &[]),
            package("npm", "buffer", &[]),
        ],
    );
    assert_eq!(
        found.sites,
        [
            site("web/src/date.ts", 1, "date-fns/format", "date-fns"),
            site("web/src/http.ts", 1, "node", "@types/node"),
            site("web/src/http.ts", 2, "@scope/http/client", "@scope/http"),
            site("web/src/shims.ts", 2, "buffer", "buffer"),
        ],
        "a built-in's bare name can be a bundled npm package, node: never is; \
         installed packages and coverage output are not scanned"
    );
    assert_eq!(
        file(&found.files, "web/src/main.mjs").unwrap().chain,
        [
            hop("web/src/main.mjs", 1),
            hop("web/src/app.ts", 1),
            hop("web/src/ui/index.tsx", 1),
            hop("web/src/date.ts", 1),
        ],
        ".js names its TypeScript source; a directory its index"
    );
    assert_eq!(
        file(&found.files, "web/test/app.test.ts").unwrap().packages,
        ["date-fns"],
        "a mock of a first-party module is an edge"
    );
    assert_eq!(
        file(&found.files, "web/test/date.test.js").unwrap().chain,
        [hop("web/test/date.test.js", 1), hop("web/src/date.ts", 1)],
        "jest.mock too"
    );
    assert_eq!(found.files.len(), 8, "{:?}", found.files);
    assert_eq!(found.warnings.len(), 1, "{:?}", found.warnings);
    assert!(found.warnings[0].contains("1 "), "{:?}", found.warnings);
}

#[test]
fn js_tsconfig_paths_resolve_through_relative_extends_and_base_url() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        &[
            ("web/package.json", r#"{"name": "web"}"#),
            (
                "web/tsconfig.base.json",
                r##"{
  // shared options, JSONC like every tsconfig
  "extends": "@tsconfig/node20/tsconfig.json",
  "compilerOptions": {
    "paths": { "@/*": ["./src/*"], "#config": ["./config/index.ts"], },
    /* trailing commas too */
  },
}"##,
            ),
            (
                "web/app/tsconfig.json",
                r#"{ "extends": ["../tsconfig.base"] }"#,
            ),
            ("web/src/store.ts", "import { createStore } from 'zustand';\n"),
            ("web/config/index.ts", "import 'zustand/middleware';\n"),
            (
                "web/app/page.ts",
                "import { s } from '@/store';\nimport c from '#config';\nimport { z } from 'zod';\n",
            ),
            (
                "web/tools/tsconfig.json",
                "\u{feff}{ \"compilerOptions\": { \"baseUrl\": \"./lib\", \"paths\": { \"*\": [\"./types/*\"] } } }",
            ),
            ("web/tools/lib/helpers/format.ts", "import 'zustand';\n"),
            (
                "web/tools/cli.ts",
                "import { f } from 'helpers/format';\nimport { z } from 'zod';\n",
            ),
            ("web/other/page.ts", "import { s } from '@/store';\n"),
        ],
    );
    let found = js_usages(
        root,
        vec![package("npm", "zustand", &[]), package("npm", "zod", &[])],
    );
    assert_eq!(
        found.sites,
        [
            site("web/app/page.ts", 3, "zod", "zod"),
            site("web/config/index.ts", 1, "zustand/middleware", "zustand"),
            site("web/src/store.ts", 1, "zustand", "zustand"),
            site("web/tools/cli.ts", 2, "zod", "zod"),
            site("web/tools/lib/helpers/format.ts", 1, "zustand", "zustand"),
        ],
        "an alias or a file under baseUrl is first-party; a pattern that names \
         no file falls through"
    );
    let page = file(&found.files, "web/app/page.ts").unwrap();
    assert_eq!(page.packages, ["zod", "zustand"]);
    assert_eq!(
        file(&found.files, "web/tools/cli.ts").unwrap().packages,
        ["zod", "zustand"],
        "baseUrl, from a tsconfig with a byte order mark"
    );
    assert!(
        file(&found.files, "web/other/page.ts").is_none(),
        "no tsconfig governs web/other, so @/store is a package"
    );
    assert!(found.warnings.is_empty(), "{:?}", found.warnings);
}

#[test]
fn js_workspace_packages_resolve_to_their_sources_not_their_build_output() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        &[
            (
                "web/package.json",
                r#"{"name": "web", "workspaces": ["packages/*"]}"#,
            ),
            (
                "web/app.ts",
                "import { Button } from '@demo/ui';\nimport { fmt } from '@demo/utils/fmt';\nimport { u } from '@demo/utils';\n",
            ),
            ("web/old.ts", "import old from 'legacy/thing';\n"),
            (
                "web/packages/ui/package.json",
                r#"{"name": "@demo/ui", "main": "./dist/ui.kit.js"}"#,
            ),
            ("web/packages/ui/src/ui.kit.ts", "export * from './Button';\n"),
            ("web/packages/plain/package.json", r#"{"name": "plain"}"#),
            ("web/packages/plain/index.ts", "export const p = 1;\n"),
            ("web/packages/plain.ts", "import clsx from 'clsx';\n"),
            ("web/plain-user.ts", "import { p } from 'plain';\n"),
            ("web/old-too.ts", "import old from 'legacy/thing';\n"),
            ("web/packages/ui/src/Button.tsx", "import clsx from 'clsx';\n"),
            (
                "web/packages/utils/package.json",
                r#"{"name": "@demo/utils", "exports": {".": {"types": "./src/index.ts", "default": "./dist/index.js"}, "./fmt": "./src/fmt.ts"}}"#,
            ),
            ("web/packages/utils/src/index.ts", "export const u = 1;\n"),
            ("web/packages/utils/src/fmt.ts", "import { format } from 'date-fns';\n"),
            (
                "web/packages/legacy/package.json",
                r#"{"name": "legacy", "main": "lib/main.js"}"#,
            ),
            ("web/packages/legacy/lib-src/a.js", "require('clsx');\n"),
        ],
    );
    let found = js_usages(
        root,
        vec![
            package("npm", "clsx", &[]),
            package("npm", "date-fns", &[]),
            package("npm", "legacy", &[]),
        ],
    );
    assert_eq!(
        found.sites,
        [
            site("web/packages/legacy/lib-src/a.js", 1, "clsx", "clsx"),
            site("web/packages/plain.ts", 1, "clsx", "clsx"),
            site("web/packages/ui/src/Button.tsx", 1, "clsx", "clsx"),
            site("web/packages/utils/src/fmt.ts", 1, "date-fns", "date-fns"),
        ],
        "a workspace package's name is never an affected package"
    );
    assert_eq!(
        file(&found.files, "web/app.ts").unwrap().chain,
        [
            hop("web/app.ts", 2),
            hop("web/packages/utils/src/fmt.ts", 1)
        ],
        "an exports subpath"
    );
    assert_eq!(
        file(&found.files, "web/app.ts").unwrap().packages,
        ["clsx", "date-fns"],
        "main in unbuilt dist/ names its source in src/"
    );
    assert!(
        file(&found.files, "web/plain-user.ts").is_none(),
        "a package without an entry is its index, not a file beside it"
    );
    assert!(file(&found.files, "web/packages/utils/src/index.ts").is_none());
    assert_eq!(
        file(&found.files, "web/old.ts").unwrap().packages,
        ["clsx"],
        "an entry that cannot be found reaches every file of the package"
    );
    assert_eq!(
        found.warnings.len(),
        1,
        "once per specifier: {:?}",
        found.warnings
    );
    assert!(
        found.warnings[0].contains("legacy/thing"),
        "{:?}",
        found.warnings
    );
}

#[test]
fn a_workspace_member_that_is_itself_a_package_is_imported_by_its_directory_name() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        &[
            (
                "svc/pyproject.toml",
                "[project]\nname = \"svc\"\n[tool.uv.workspace]\nmembers = [\"core\", \"api\"]\n",
            ),
            ("svc/core/pyproject.toml", "[project]\nname = \"core\"\n"),
            ("svc/core/__init__.py", ""),
            ("svc/core/storage/__init__.py", ""),
            ("svc/core/storage/client.py", "import idna\n"),
            ("svc/api/pyproject.toml", "[project]\nname = \"api\"\n"),
            ("svc/api/__init__.py", ""),
            (
                "svc/api/main.py",
                "from core.storage.client import connect\nfrom api import deps\n",
            ),
            ("svc/api/deps.py", "from util.text import slug\n"),
            (
                "svc/packages/util/pyproject.toml",
                "[project]\nname = \"util\"\n",
            ),
            ("svc/packages/util/__init__.py", ""),
            ("svc/packages/util/text.py", "import idna\n"),
        ],
    );
    let found = usages(
        root,
        &target(),
        &affected(vec![
            package("pypi", "idna", &[]),
            package("pypi", "core", &[]),
            package("pypi", "api", &[]),
        ]),
        &ImportNames::default(),
    )
    .unwrap();
    assert_eq!(
        found.sites,
        [
            site("svc/core/storage/client.py", 1, "idna", "idna"),
            site("svc/packages/util/text.py", 1, "idna", "idna"),
        ],
        "imports of the members are first-party"
    );
    assert_eq!(
        file(&found.files, "svc/api/deps.py").unwrap().chain,
        [
            hop("svc/api/deps.py", 1),
            hop("svc/packages/util/text.py", 1)
        ],
        "a member below its parent's root is rooted at that parent"
    );
    assert_eq!(
        file(&found.files, "svc/api/main.py").unwrap().chain,
        [
            hop("svc/api/main.py", 1),
            hop("svc/core/storage/client.py", 1)
        ]
    );
}

fn reference(
    file: &str,
    line: usize,
    package: &str,
    name: &str,
    kind: ByNameKind,
) -> ByNameReference {
    ByNameReference {
        file: file.into(),
        line,
        package: package.into(),
        name: name.into(),
        kind,
    }
}

#[test]
fn packages_named_in_source_strings_are_references_that_reach_their_importers() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        &[
            ("svc/pyproject.toml", "[project]\nname = \"svc\"\n"),
            (
                "svc/build.py",
                "import subprocess, sys\n\ndef build():\n    \"\"\"Run nuitka.\"\"\"\n    subprocess.run([sys.executable, \"-m\", \"nuitka\", \"--version\"])\n    subprocess.run(\"ruff check .\", shell=True)\n    return version(\"PyYAML\"), \"requests are fine\"\n",
            ),
            (
                "svc/main.py",
                "from build import build\nAPP = \"svc\"\n\ndef handler(session: \"Requests\", key=\"RUFF\"):\n    app.run(\"ruff is fast\")\n    importlib.import_module(\n        \"nuitka\"\n    )\n",
            ),
        ],
    );
    let found = usages(
        root,
        &target(),
        &affected(vec![
            package("pypi", "nuitka", &[]),
            package("pypi", "ruff", &[]),
            package("pypi", "pyyaml", &[]),
            package("pypi", "requests", &[]),
            package("pypi", "svc", &[]),
        ]),
        &ImportNames::default(),
    )
    .unwrap();
    assert_eq!(
        found.sites,
        [site("svc/main.py", 6, "nuitka", "nuitka")],
        "an import's specifier is a usage site only"
    );
    assert_eq!(
        found.by_name,
        [
            reference("svc/build.py", 5, "nuitka", "nuitka", ByNameKind::Source),
            reference("svc/build.py", 6, "ruff", "ruff", ByNameKind::Source),
            reference("svc/build.py", 7, "pyyaml", "PyYAML", ByNameKind::Source),
        ],
        "a docstring or a sentence names nothing"
    );
    assert_eq!(
        file(&found.files, "svc/main.py").unwrap().packages,
        ["nuitka", "pyyaml", "ruff"],
        "a reference seeds the reverse walk"
    );
}

#[test]
fn pixi_tasks_name_packages_by_their_executables() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let env = "svc/.pixi/envs/default";
    write(
        root,
        &[
            (
                "svc/pixi.toml",
                "[workspace]\nname = \"svc\"\n\n[tasks]\nlint = \"prek run --all-files\"\nbuild = { cmd = \"python -m nuitka main.py\", depends-on = [\"lint\"] }\n\n[feature.test.tasks]\ntest = { cmd = [\"py.test\", \"-x\"] }\nall = { depends-on = [\"lint\", \"test\"] }\n\n[tasks.self-check]\ncmd = \"svc --check\"\n\n[tasks.types]\ncmd = \"dmypy run\"\n\n[feature.ci.dependencies]\npytest = \"*\"\n\n[feature.ci.tasks]\ntest = \"pixi run -e ci python -X dev -m ruff check\"\ndocs = \"\"\"\nsphinx-build docs out\npixi run test\n\"\"\"\n",
            ),
            (
                &format!("{env}/conda-meta/prek-0.5.3-h0.json"),
                r#"{"name": "prek", "files": ["bin/prek"]}"#,
            ),
            (
                &format!("{env}/lib/python3.12/site-packages/pytest-8.3.0.dist-info/RECORD"),
                "pytest/__init__.py,,\n",
            ),
            (
                &format!("{env}/lib/python3.12/site-packages/pytest-8.3.0.dist-info/entry_points.txt"),
                "[console_scripts]\npy.test = pytest:console_main\npytest = pytest:console_main\n",
            ),
        ],
    );
    let pixi = Target {
        id: "pixi:svc/pixi.toml".into(),
        manager: "pixi".into(),
        manifest: "svc/pixi.toml".into(),
    };
    let names = ImportNames::read(root, &pixi);
    let found = usages(
        root,
        &pixi,
        &affected(vec![
            package("conda", "prek", &[]),
            package("pypi", "nuitka", &[]),
            package("pypi", "pytest", &[]),
            package("pypi", "svc", &[]),
            package("pypi", "mypy", &[]),
            package("pypi", "ruff", &[]),
            package("pypi", "sphinx", &[]),
        ]),
        &names,
    )
    .unwrap();
    assert_eq!(
        found.by_name,
        [
            reference("svc/pixi.toml", 5, "prek", "prek", ByNameKind::Task),
            reference("svc/pixi.toml", 6, "nuitka", "nuitka", ByNameKind::Task),
            reference("svc/pixi.toml", 9, "pytest", "py.test", ByNameKind::Task),
            reference("svc/pixi.toml", 15, "mypy", "dmypy", ByNameKind::Task),
            reference("svc/pixi.toml", 22, "ruff", "ruff", ByNameKind::Task),
            reference(
                "svc/pixi.toml",
                23,
                "sphinx",
                "sphinx-build",
                ByNameKind::Task
            ),
        ],
        "the target's own project is never a reference"
    );
}

#[test]
fn package_scripts_name_packages_through_their_installed_bins_and_wrappers() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        &[
            (
                "web/package.json",
                r#"{
  "name": "web",
  "scripts": {
    "build": "tsc -p . && vite build",
    "test": "vitest run",
    "e2e": "npx playwright test",
    "lint": "NODE_ENV=ci eslint .",
    "unit": "cross-env CI=1 npx -p @demo/jest-preset jest --ci",
    "check": "yarn tsc --noEmit",
    "all": "npm run build && npm run unit && npm test"
  }
}"#,
            ),
            (
                "web/node_modules/typescript/package.json",
                r#"{"name": "typescript", "bin": {"tsc": "./bin/tsc", "tsserver": "./bin/tsserver"}}"#,
            ),
            (
                "web/src/run.ts",
                "await execa(\"npx\", [\"vitest\", \"run\"]);\n",
            ),
        ],
    );
    let found = js_usages(
        root,
        vec![
            package("npm", "typescript", &[]),
            package("npm", "vitest", &[]),
            package("npm", "@playwright/test", &[]),
            package("npm", "eslint", &[]),
            package("npm", "jest", &[]),
            package("npm", "@demo/jest-preset", &[]),
        ],
    );
    assert_eq!(
        found.by_name,
        [
            reference("web/package.json", 4, "typescript", "tsc", ByNameKind::Task),
            reference("web/package.json", 5, "vitest", "vitest", ByNameKind::Task),
            reference(
                "web/package.json",
                6,
                "@playwright/test",
                "playwright",
                ByNameKind::Task
            ),
            reference("web/package.json", 7, "eslint", "eslint", ByNameKind::Task),
            reference("web/package.json", 8, "jest", "jest", ByNameKind::Task),
            reference("web/package.json", 9, "typescript", "tsc", ByNameKind::Task),
            reference("web/src/run.ts", 1, "vitest", "vitest", ByNameKind::Source),
        ]
    );
}

//! The import scanners: every module a source file imports, with its line
//! and how it is imported, read with tree-sitter.
use depsmith_core::impact::{
    js_imports, js_strings, python_imports, python_strings, Dialect, Import, ImportKind, Literal,
};

fn import(module: &str, names: &[&str], level: usize, line: usize, kind: ImportKind) -> Import {
    Import {
        module: module.into(),
        names: names.iter().map(|n| (*n).into()).collect(),
        level,
        line,
        kind,
    }
}

#[test]
fn python_statements_name_absolute_and_relative_modules() {
    let source = "\
import os, idna.core as core
from requests import Session, adapters
from . import sibling
from ..pkg.mod import thing

def handler():
    import yaml
";
    let scan = python_imports(source);
    assert_eq!(
        scan.imports,
        [
            import("os", &[], 0, 1, ImportKind::Import),
            import("idna.core", &[], 0, 1, ImportKind::Import),
            import("requests", &["Session", "adapters"], 0, 2, ImportKind::From),
            import("", &["sibling"], 1, 3, ImportKind::From),
            import("pkg.mod", &["thing"], 2, 4, ImportKind::From),
            import("yaml", &[], 0, 7, ImportKind::Import),
        ]
    );
    assert!(scan.unparsed.is_empty());
}

#[test]
fn python_calls_and_plugin_lists_name_modules_by_string() {
    let source = r#"
import importlib
from importlib import import_module
import pytest

backend = importlib.import_module("svc.backends.redis")
codec = import_module('idna.codec')
legacy = __import__("yaml")
skipped = importlib.import_module(name)
numpy = pytest.importorskip("numpy")
pytest_plugins = ["svc.fixtures", "pytest_asyncio"]
"#;
    let found: Vec<_> = python_imports(source)
        .imports
        .into_iter()
        .filter(|i| !matches!(i.kind, ImportKind::Import | ImportKind::From))
        .collect();
    assert_eq!(
        found,
        [
            import("svc.backends.redis", &[], 0, 6, ImportKind::Dynamic),
            import("idna.codec", &[], 0, 7, ImportKind::Dynamic),
            import("yaml", &[], 0, 8, ImportKind::Dynamic),
            import("numpy", &[], 0, 10, ImportKind::ImportOrSkip),
            import("svc.fixtures", &[], 0, 11, ImportKind::Plugin),
            import("pytest_asyncio", &[], 0, 11, ImportKind::Plugin),
        ],
        "a computed module name is not a usage site"
    );
    let single = python_imports("pytest_plugins = 'svc.fixtures'\n");
    assert_eq!(
        single.imports,
        [import("svc.fixtures", &[], 0, 1, ImportKind::Plugin)]
    );
}

#[test]
fn python_syntax_the_parser_does_not_know_keeps_the_imports_around_it() {
    let source = "import idna\n\ndef broken(:\n    pass\n\nimport yaml\n";
    let scan = python_imports(source);
    let modules: Vec<_> = scan.imports.iter().map(|i| i.module.as_str()).collect();
    assert_eq!(modules, ["idna", "yaml"]);
    assert_eq!(scan.unparsed, [3]);
}

#[test]
fn js_statements_name_specifiers_with_their_bindings() {
    let source = r#"import React, { useState as state, useEffect } from "react";
import * as path from 'node:path';
import "./polyfills";
import type { Config } from "@scope/config/types";
export { helper, default as main } from "./helpers.js";
export * from "../shared";
const fs = require("fs-extra");
const where = require.resolve(`lodash/fp`);
"#;
    let scan = js_imports(source, Dialect::TypeScript);
    assert_eq!(
        scan.imports,
        [
            import(
                "react",
                &["default", "useState", "useEffect"],
                0,
                1,
                ImportKind::Import
            ),
            import("node:path", &["*"], 0, 2, ImportKind::Import),
            import("./polyfills", &[], 0, 3, ImportKind::Import),
            import("@scope/config/types", &["Config"], 0, 4, ImportKind::Import),
            import(
                "./helpers.js",
                &["helper", "default"],
                0,
                5,
                ImportKind::Export
            ),
            import("../shared", &["*"], 0, 6, ImportKind::Export),
            import("fs-extra", &[], 0, 7, ImportKind::Require),
            import("lodash/fp", &[], 0, 8, ImportKind::Require),
        ]
    );
    assert!(scan.unparsed.is_empty());
}

#[test]
fn js_dynamic_imports_mocks_and_references_name_modules_by_string() {
    let source = r#"/// <reference types="vite/client" />
/// <reference path="globals.d.ts" />
import { vi } from "vitest";

vi.mock("./db", () => ({}));
jest.mock("axios");
jest.doMock("../cache");
const real = jest.requireActual("lodash");
const actual = await vi.importActual("./clock");
const lazy = await import("./routes/admin");
const computed = await import(name);
const templated = require(`./locales/${lang}`);
"#;
    let scan = js_imports(source, Dialect::TypeScript);
    assert_eq!(
        scan.imports,
        [
            import("vite/client", &[], 0, 1, ImportKind::Reference),
            import("./globals.d.ts", &[], 0, 2, ImportKind::Reference),
            import("vitest", &["vi"], 0, 3, ImportKind::Import),
            import("./db", &[], 0, 5, ImportKind::Mock),
            import("axios", &[], 0, 6, ImportKind::Mock),
            import("../cache", &[], 0, 7, ImportKind::Mock),
            import("lodash", &[], 0, 8, ImportKind::Mock),
            import("./clock", &[], 0, 9, ImportKind::Mock),
            import("./routes/admin", &[], 0, 10, ImportKind::Dynamic),
        ],
        "a computed specifier is not a usage site; a reference path is relative"
    );
}

#[test]
fn jsx_and_tsx_parse_and_unknown_syntax_keeps_the_imports_around_it() {
    let jsx = "import Button from './Button';\nexport const App = () => <Button label=\"x\" />;\n";
    let scan = js_imports(jsx, Dialect::JavaScript);
    assert_eq!(
        scan.imports,
        [import("./Button", &["default"], 0, 1, ImportKind::Import)]
    );
    assert!(scan.unparsed.is_empty());
    let tsx = "import type { FC } from 'react';\nconst A: FC = () => <div>{1 as number}</div>;\n";
    let scan = js_imports(tsx, Dialect::Tsx);
    assert_eq!(
        scan.imports,
        [import("react", &["FC"], 0, 1, ImportKind::Import)]
    );
    assert!(scan.unparsed.is_empty(), "{:?}", scan.unparsed);
    let broken = "import a from 'a';\nconst x = ;\nimport b from 'b';\n";
    let scan = js_imports(broken, Dialect::JavaScript);
    let modules: Vec<_> = scan.imports.iter().map(|i| i.module.as_str()).collect();
    assert_eq!(modules, ["a", "b"]);
    assert_eq!(scan.unparsed, [2]);
}

#[test]
fn js_imports_inside_and_after_broken_syntax_still_count_with_one_warning_each() {
    let scan = js_imports(
        "import { a from 'x';\nimport b from 'y';\n",
        Dialect::TypeScript,
    );
    let modules: Vec<_> = scan.imports.iter().map(|i| i.module.as_str()).collect();
    assert_eq!(modules, ["y"], "{scan:?}");
    assert_eq!(scan.unparsed, [1]);
    let scan = js_imports("const x = (\nimport a from 'a';\n", Dialect::JavaScript);
    assert_eq!(
        scan.unparsed,
        [2],
        "an import inside an open expression is lost, with one warning"
    );
    let scan = js_imports(
        "import x = require('legacy');\nvi.doMock('./flags');\n",
        Dialect::TypeScript,
    );
    assert_eq!(
        scan.imports,
        [
            import("legacy", &["*"], 0, 1, ImportKind::Import),
            import("./flags", &[], 0, 2, ImportKind::Mock),
        ]
    );
    let escaped = js_imports(
        "require('\\x2e/a');\nimport '\\u0040scope/pkg';\n",
        Dialect::JavaScript,
    );
    let modules: Vec<_> = escaped.imports.iter().map(|i| i.module.as_str()).collect();
    assert_eq!(modules, ["./a", "@scope/pkg"], "escapes are decoded");
    let spaced = js_imports("/// <reference types = \"jest\" />\n", Dialect::TypeScript);
    assert_eq!(
        spaced.imports,
        [import("jest", &[], 0, 1, ImportKind::Reference)]
    );
}

fn literal(values: &[Option<&str>], line: usize) -> Literal {
    Literal {
        values: values.iter().map(|v| v.map(str::to_owned)).collect(),
        line,
        command: false,
    }
}

fn command(values: &[Option<&str>], line: usize) -> Literal {
    Literal {
        command: true,
        ..literal(values, line)
    }
}

#[test]
fn python_strings_and_lists_of_them_are_literals_but_docstrings_are_not() {
    let source = r#""""A module docstring naming nuitka."""
import subprocess, sys

def build():
    """Build with nuitka."""
    subprocess.run([sys.executable, "-m", "nuitka", "--version"])
    name = f"{prefix}-yaml"
    app: "Flask" = make()
    return version("pyyaml"), ("ruff", "check")
"#;
    assert_eq!(
        python_strings(source),
        [
            command(&[None, Some("-m"), Some("nuitka"), Some("--version")], 6),
            literal(&[Some("-m")], 6),
            literal(&[Some("nuitka")], 6),
            literal(&[Some("--version")], 6),
            literal(&[Some("pyyaml")], 9),
            literal(&[Some("ruff"), Some("check")], 9),
            literal(&[Some("ruff")], 9),
            literal(&[Some("check")], 9),
        ],
        "an interpolated string or an annotation is not a literal; a launched one is a command"
    );
}

#[test]
fn js_strings_and_arrays_of_them_are_literals_but_templates_with_substitutions_are_not() {
    let source = r#"import { execa } from "execa";
await execa("npx", ["vitest", "run"]);
const env = `jsdom`;
const dynamic = `${name}-loader`;
"#;
    assert_eq!(
        js_strings(source, Dialect::TypeScript),
        [
            command(&[Some("npx")], 2),
            literal(&[Some("vitest"), Some("run")], 2),
            literal(&[Some("vitest")], 2),
            literal(&[Some("run")], 2),
            literal(&[Some("jsdom")], 3),
        ],
        "an import's specifier is a usage site, not a literal"
    );
}

#[test]
fn python_imports_after_broken_syntax_still_count_with_one_warning() {
    let scan = python_imports("import idna\nx = (\n\ndef f(:\n    pass\nimport yaml\n");
    let modules: Vec<_> = scan.imports.iter().map(|i| i.module.as_str()).collect();
    assert!(
        modules.contains(&"idna") && modules.contains(&"yaml"),
        "{scan:?}"
    );
    let mut lines = scan.unparsed.clone();
    lines.dedup();
    assert_eq!(scan.unparsed, lines, "one warning per line");
}

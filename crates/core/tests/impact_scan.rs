//! The import scanners: every module a source file imports, with its line
//! and how it is imported, read with tree-sitter.
use depsmith_core::impact::{python_imports, Import, ImportKind};

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

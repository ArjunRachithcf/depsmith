//! What an import scanner reports for one source file.
use serde::{Deserialize, Serialize};

/// How a source file imports a module.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ImportKind {
    /// `import a.b`.
    Import,
    /// `from a import b`.
    From,
    /// A call that imports a module named by a string, such as
    /// `importlib.import_module("a")`.
    Dynamic,
    /// A pytest plugin listed in `pytest_plugins`.
    Plugin,
    /// `pytest.importorskip("a")`: tests that skip without the module.
    ImportOrSkip,
    /// `require("a")` or `require.resolve("a")`.
    Require,
    /// `export … from "a"`.
    Export,
    /// A test runner's mock of a module, such as `vi.mock("a")`.
    Mock,
    /// A triple-slash `/// <reference types="a" />` or `path` directive.
    Reference,
}

/// A module one source file imports.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Import {
    /// The module as written. For Python, without leading dots: `a.b` for
    /// `import a.b`, empty for `from . import b`, with `level` counting
    /// them. For JavaScript or TypeScript, the specifier exactly as
    /// written, such as `./util.js` or `@scope/pkg`.
    pub module: String,
    /// The names imported from it (`b` and `c` in `from a import b, c`),
    /// which can be submodules.
    pub names: Vec<String>,
    /// For Python, how many packages up a relative import starts: 1 for
    /// `from .`, 0 for an absolute import. Always `0` for JavaScript or
    /// TypeScript.
    pub level: usize,
    /// The 1-based line the import is on.
    pub line: usize,
    /// How it is imported.
    pub kind: ImportKind,
}

impl Import {
    /// An import of `module` alone, on `line`: empty names and level `0`.
    pub(crate) fn of(module: String, line: usize, kind: ImportKind) -> Self {
        Self {
            module,
            names: vec![],
            level: 0,
            line,
            kind,
        }
    }
}

/// The imports of one source file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scan {
    /// Each import, in source order.
    pub imports: Vec<Import>,
    /// The 1-based first line of each region the parser could not read.
    /// Imports inside them are not reported.
    pub unparsed: Vec<usize>,
}

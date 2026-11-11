//! What an import scanner reports for one source file, and the parsing it
//! shares: regions the parser cannot read are read again from their second
//! line.
use super::tables::LAUNCHERS;
use serde::{Deserialize, Serialize};
use tree_sitter::{Node, Parser};

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

/// A string literal, or a list, tuple or array of them, as a source file
/// writes it: a package can be named in one, such as `"-m", "nuitka"`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Literal {
    /// Its strings, in order; `None` for an element that is not a string
    /// literal, such as `sys.executable`. A plain string has one.
    pub values: Vec<Option<String>>,
    /// The 1-based line it starts on.
    pub line: usize,
    /// Whether it is the command a call starts a process with, such as the
    /// first argument of `subprocess.run` or `execa`, so a single string is
    /// read as a command line.
    pub command: bool,
}

/// Whether `node`, a literal, is the first argument of a call to one of the
/// [`LAUNCHERS`]: one named exactly so, or ending in `.` and a qualified
/// one, such as `subprocess.run` in `sp.subprocess.run`.
pub(super) fn launched(node: Node, source: &[u8]) -> bool {
    let Some(arguments) = node
        .parent()
        .filter(|p| p.kind() == "arguments" || p.kind() == "argument_list")
    else {
        return false;
    };
    if arguments
        .named_child(0)
        .is_none_or(|first| first.id() != node.id())
    {
        return false;
    }
    arguments
        .parent()
        .and_then(|call| call.child_by_field_name("function"))
        .and_then(|function| function.utf8_text(source).ok())
        .is_some_and(|name| {
            LAUNCHERS.iter().any(|launcher| {
                name == *launcher
                    || (launcher.contains('.')
                        && name
                            .strip_suffix(launcher)
                            .is_some_and(|before| before.ends_with('.')))
            })
        })
}

/// What a scanner finds in one file: its imports and its literals.
#[derive(Debug, Default)]
pub(super) struct Found {
    pub(super) scan: Scan,
    pub(super) literals: Vec<Literal>,
}

/// Visits one node of a region starting on 0-based line `offset`, recording
/// in its `retry` argument the bytes of the first region it cannot read that
/// spans several lines.
pub(super) type Visit = fn(Node, &[u8], usize, &mut Option<(usize, usize)>, &mut Found);

/// How many times a region the parser could not read is parsed again from
/// its second line.
const RECOVERIES: usize = 8;

/// Scans `source` with `parser`, every region by `visit`, in line order.
pub(super) fn scan(parser: &mut Parser, source: &str, visit: Visit) -> Found {
    let mut found = Found::default();
    region(parser, source, 0, RECOVERIES, visit, &mut found);
    // Regions read again report after the rest of the file.
    found.scan.imports.sort_by_key(|import| import.line);
    found.literals.sort_by_key(|literal| literal.line);
    found.scan.unparsed.sort_unstable();
    found.scan.unparsed.dedup();
    found
}

fn region(
    parser: &mut Parser,
    text: &str,
    offset: usize,
    recoveries: usize,
    visit: Visit,
    found: &mut Found,
) {
    let Some(tree) = parser.parse(text, None) else {
        return;
    };
    let mut retry = None;
    visit(tree.root_node(), text.as_bytes(), offset, &mut retry, found);
    // A region the parser could not read can swallow the statements after
    // it, so the rest of it is read again on its own.
    if let Some((start, end)) = retry.filter(|_| recoveries > 0) {
        if let Some(next) = text[start..end].find('\n') {
            let from = start + next + 1;
            let line = offset + text[..from].matches('\n').count();
            region(parser, &text[from..end], line, recoveries - 1, visit, found);
        }
    }
}

/// Whether `node` is where the parser could not read the source, which its
/// visitor does not descend into: its line is listed as unparsed, and a
/// region spanning several lines is kept in `retry` to read again.
pub(super) fn unreadable(
    node: Node,
    offset: usize,
    retry: &mut Option<(usize, usize)>,
    found: &mut Found,
) -> bool {
    if !(node.is_error() || node.is_missing()) {
        return false;
    }
    found
        .scan
        .unparsed
        .push(offset + node.start_position().row + 1);
    if node.is_error() && node.end_position().row > node.start_position().row {
        retry.get_or_insert((node.start_byte(), node.end_byte()));
        return true;
    }
    node.is_missing()
}

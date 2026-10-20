//! The JavaScript and TypeScript import scanner.
use super::imports::{Import, ImportKind, Scan};
use std::path::Path;
use tree_sitter::{Node, Parser};

/// The grammar a JavaScript or TypeScript source file is parsed with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    /// `.js`, `.mjs`, `.cjs` and `.jsx`, JSX included.
    JavaScript,
    /// `.ts`, `.mts` and `.cts`, declaration files included.
    TypeScript,
    /// `.tsx`.
    Tsx,
}

impl Dialect {
    /// The dialect of `path` by its extension, or `None` when it is not a
    /// JavaScript or TypeScript file.
    pub fn of(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()? {
            "js" | "mjs" | "cjs" | "jsx" => Some(Self::JavaScript),
            "ts" | "mts" | "cts" => Some(Self::TypeScript),
            "tsx" => Some(Self::Tsx),
            _ => None,
        }
    }
}

/// Every module `source`, a file in `dialect`, imports, by its specifier as
/// written.
///
/// Only specifiers written as a string, or a template without
/// substitutions, are reported. Where the parser meets syntax it does not
/// know, the first line of the region is listed in [`Scan::unparsed`], and
/// the imports it can still recognize are reported.
pub fn js_imports(source: &str, dialect: Dialect) -> Scan {
    let language = match dialect {
        Dialect::JavaScript => tree_sitter_javascript::LANGUAGE,
        Dialect::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
        Dialect::Tsx => tree_sitter_typescript::LANGUAGE_TSX,
    };
    let mut parser = Parser::new();
    parser
        .set_language(&language.into())
        .expect("the bundled grammars match tree-sitter");
    let mut scan = Scan::default();
    region(&mut parser, source, 0, RECOVERIES, &mut scan);
    // Regions read again report after the rest of the file.
    scan.imports.sort_by_key(|import| import.line);
    scan.unparsed.sort_unstable();
    scan.unparsed.dedup();
    scan
}

/// How many times a region the parser could not read is parsed again from
/// its second line.
const RECOVERIES: usize = 8;

/// Scans `text`, which starts on the 0-based line `offset` of the file.
fn region(parser: &mut Parser, text: &str, offset: usize, recoveries: usize, scan: &mut Scan) {
    let Some(tree) = parser.parse(text, None) else {
        return;
    };
    let mut retry = None;
    visit(tree.root_node(), text.as_bytes(), offset, &mut retry, scan);
    // A region the parser could not read can swallow the statements after
    // it, so the rest of it is read again on its own.
    if let Some((start, end)) = retry.filter(|_| recoveries > 0) {
        if let Some(next) = text[start..end].find('\n') {
            let from = start + next + 1;
            let line = offset + text[..from].matches('\n').count();
            region(parser, &text[from..end], line, recoveries - 1, scan);
        }
    }
}

/// Visits `node` of a region starting on line `offset`, recording in
/// `retry` the bytes of the first region it could not read that spans
/// several lines.
fn visit(
    node: Node,
    source: &[u8],
    offset: usize,
    retry: &mut Option<(usize, usize)>,
    scan: &mut Scan,
) {
    let line = offset + node.start_position().row + 1;
    if node.is_error() || node.is_missing() {
        scan.unparsed.push(line);
        if node.is_error() && node.end_position().row > node.start_position().row {
            retry.get_or_insert((node.start_byte(), node.end_byte()));
            return;
        }
        if node.is_missing() {
            return;
        }
    }
    match node.kind() {
        "import_statement" | "export_statement" => {
            let kind = if node.kind() == "import_statement" {
                ImportKind::Import
            } else {
                ImportKind::Export
            };
            let source_node = node.child_by_field_name("source").or_else(|| {
                let mut cursor = node.walk();
                let clause = node
                    .children(&mut cursor)
                    .find(|c| c.kind() == "import_require_clause");
                clause.and_then(|c| c.child_by_field_name("source"))
            });
            if let Some(module) = source_node.and_then(|s| literal(s, source)) {
                let mut names = vec![];
                bindings(node, source, &mut names);
                scan.imports.push(Import {
                    names,
                    ..Import::of(module, line, kind)
                });
            }
        }
        "comment" => {
            if let Some(import) = reference(node, source, line) {
                scan.imports.push(import);
            }
        }
        "call_expression" => {
            if let Some(import) = call(node, source, line) {
                scan.imports.push(import);
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit(child, source, offset, retry, scan);
    }
}

/// The names an `import` or `export … from` statement takes: `default` for
/// a default import, `*` for a namespace.
fn bindings(node: Node, source: &[u8], names: &mut Vec<String>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "import_clause" | "named_imports" | "export_clause" => bindings(child, source, names),
            "identifier" if node.kind() == "import_clause" => names.push("default".into()),
            "namespace_import" | "namespace_export" | "import_require_clause" => {
                names.push("*".into())
            }
            "*" if node.kind() == "export_statement" => names.push("*".into()),
            "import_specifier" | "export_specifier" => {
                if let Some(name) = child.child_by_field_name("name") {
                    let text = name.utf8_text(source).unwrap_or_default();
                    names.push(literal(name, source).unwrap_or_else(|| text.to_owned()));
                }
            }
            _ => {}
        }
    }
}

/// The import a call makes: `require("a")`, `require.resolve("a")`,
/// `import("a")`, or a jest or vitest mock of `a`.
fn call(node: Node, source: &[u8], line: usize) -> Option<Import> {
    let function = node.child_by_field_name("function")?;
    let kind = match function.kind() {
        "import" => ImportKind::Dynamic,
        _ => match function.utf8_text(source).ok()? {
            "require" | "require.resolve" => ImportKind::Require,
            "jest.mock" | "jest.doMock" | "jest.requireActual" | "vi.mock" | "vi.doMock"
            | "vi.importActual" => ImportKind::Mock,
            _ => return None,
        },
    };
    let first = node.child_by_field_name("arguments")?.named_child(0)?;
    Some(Import::of(literal(first, source)?, line, kind))
}

/// The module a `/// <reference types="a" />` directive names, or the file
/// of a `path` one, which is always relative (`./a.d.ts`).
fn reference(node: Node, source: &[u8], line: usize) -> Option<Import> {
    let directive = node
        .utf8_text(source)
        .ok()?
        .strip_prefix("///")?
        .trim_start()
        .strip_prefix("<reference")?;
    let value = |attribute: &str| {
        let (_, rest) = directive.split_once(attribute)?;
        let rest = rest.trim_start().strip_prefix('=')?.trim_start();
        let quote = rest.chars().next().filter(|q| matches!(q, '"' | '\''))?;
        let rest = &rest[1..];
        Some(rest[..rest.find(quote)?].to_owned())
    };
    let module = match (value("types"), value("path")) {
        (Some(types), _) => types,
        (None, Some(path)) if path.starts_with('.') || path.starts_with('/') => path,
        (None, Some(path)) => format!("./{path}"),
        (None, None) => return None,
    };
    Some(Import::of(module, line, ImportKind::Reference))
}

/// The text of `node` when it is a string literal, or a template without
/// substitutions.
fn literal(node: Node, source: &[u8]) -> Option<String> {
    if !matches!(node.kind(), "string" | "template_string") {
        return None;
    }
    let mut cursor = node.walk();
    let mut content = String::new();
    for part in node.named_children(&mut cursor) {
        match part.kind() {
            "string_fragment" => content.push_str(part.utf8_text(source).ok()?),
            "escape_sequence" => content.push(unescape(part.utf8_text(source).ok()?)?),
            _ => return None,
        }
    }
    Some(content)
}

/// The character an escape sequence such as `\n`, `\x2e` or `\u{40}`
/// stands for.
fn unescape(escape: &str) -> Option<char> {
    let body = escape.strip_prefix('\\')?;
    let hex = |digits: &str| char::from_u32(u32::from_str_radix(digits, 16).ok()?);
    match body.chars().next()? {
        'x' => hex(&body[1..]),
        'u' => hex(body[1..].trim_start_matches('{').trim_end_matches('}')),
        'n' => Some('\n'),
        't' => Some('\t'),
        'r' => Some('\r'),
        'b' => Some('\u{8}'),
        'f' => Some('\u{c}'),
        'v' => Some('\u{b}'),
        '0' => Some('\0'),
        other => Some(other),
    }
}

//! The Python import scanner.
use super::imports::{launched, scan, unreadable, Found, Import, ImportKind, Literal, Scan};
use tree_sitter::{Node, Parser};

/// Every module `source`, a Python file, imports.
///
/// Where the parser meets syntax it does not know, the first line of the
/// region is listed in [`Scan::unparsed`], and the imports it can still
/// recognize are reported.
pub fn python_imports(source: &str) -> Scan {
    python_scan(source).scan
}

/// Every string literal, and list or tuple of them, in `source`, a Python
/// file, by line. Docstrings and interpolated strings are not literals.
pub fn python_strings(source: &str) -> Vec<Literal> {
    python_scan(source).literals
}

/// The imports and literals of `source`.
pub(super) fn python_scan(source: &str) -> Found {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .expect("the bundled Python grammar matches tree-sitter");
    scan(&mut parser, source, visit)
}

fn visit(
    node: Node,
    source: &[u8],
    offset: usize,
    retry: &mut Option<(usize, usize)>,
    found: &mut Found,
) {
    let line = offset + node.start_position().row + 1;
    let text = |node: Node| node.utf8_text(source).unwrap_or_default().to_owned();
    if unreadable(node, offset, retry, found) {
        return;
    }
    let scan = &mut found.scan;
    match node.kind() {
        "import_statement" => {
            for name in node.children_by_field_name("name", &mut node.walk()) {
                scan.imports
                    .push(Import::of(text(dotted(name)), line, ImportKind::Import));
            }
            return;
        }
        "import_from_statement" => {
            let Some(module) = node.child_by_field_name("module_name") else {
                return;
            };
            let (module, level) = if module.kind() == "relative_import" {
                let mut cursor = module.walk();
                let mut level = 0;
                let mut name = String::new();
                for part in module.children(&mut cursor) {
                    match part.kind() {
                        "import_prefix" => level = text(part).len(),
                        "dotted_name" => name = text(part),
                        _ => {}
                    }
                }
                (name, level)
            } else {
                (text(module), 0)
            };
            let names = node
                .children_by_field_name("name", &mut node.walk())
                .map(|name| text(dotted(name)))
                .collect();
            scan.imports.push(Import {
                module,
                names,
                level,
                line,
                kind: ImportKind::From,
            });
            return;
        }
        "call" => {
            let function = node.child_by_field_name("function").map(text);
            let kind = match function.as_deref().and_then(|f| f.rsplit('.').next()) {
                Some("import_module" | "__import__") => Some(ImportKind::Dynamic),
                Some("importorskip") => Some(ImportKind::ImportOrSkip),
                _ => None,
            };
            let first = node
                .child_by_field_name("arguments")
                .and_then(|arguments| arguments.named_child(0));
            if let (Some(kind), Some(module)) = (kind, first.and_then(|a| literal(a, source))) {
                scan.imports.push(Import::of(module, line, kind));
            }
        }
        "assignment"
            if node
                .child_by_field_name("left")
                .is_some_and(|left| text(left) == "pytest_plugins") =>
        {
            if let Some(right) = node.child_by_field_name("right") {
                let plugins = match right.kind() {
                    "list" | "tuple" => {
                        let mut cursor = right.walk();
                        right.named_children(&mut cursor).collect()
                    }
                    _ => vec![right],
                };
                for plugin in plugins {
                    if let Some(module) = literal(plugin, source) {
                        scan.imports.push(Import::of(
                            module,
                            offset + plugin.start_position().row + 1,
                            ImportKind::Plugin,
                        ));
                    }
                }
            }
            return;
        }
        // A docstring, or any string standing alone, names nothing.
        "expression_statement"
            if node.named_child_count() == 1
                && node.named_child(0).is_some_and(|c| c.kind() == "string") =>
        {
            return;
        }
        // An annotation names a type, not a package, even as a string.
        "type" => return,
        "string" if !is_specifier(node, source) => {
            if let Some(value) = literal(node, source) {
                found.literals.push(Literal {
                    values: vec![Some(value)],
                    line,
                    command: launched(node, source),
                });
            }
        }
        "list" | "tuple" => {
            let mut cursor = node.walk();
            let values = node
                .named_children(&mut cursor)
                .map(|element| literal(element, source))
                .collect();
            found.literals.push(Literal {
                values,
                line,
                command: launched(node, source),
            });
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit(child, source, offset, retry, found);
    }
}

/// Whether string `node` names the module of an import call, such as
/// `importlib.import_module`, which is a usage site already.
fn is_specifier(node: Node, source: &[u8]) -> bool {
    node.parent()
        .filter(|arguments| arguments.kind() == "argument_list")
        .and_then(|arguments| arguments.parent())
        .and_then(|call| call.child_by_field_name("function"))
        .and_then(|function| function.utf8_text(source).ok())
        .and_then(|name| name.rsplit('.').next())
        .is_some_and(|name| matches!(name, "import_module" | "__import__" | "importorskip"))
}

/// The text of `node` when it is a plain string literal, without prefixes
/// or interpolation.
fn literal(node: Node, source: &[u8]) -> Option<String> {
    if node.kind() != "string" {
        return None;
    }
    let mut cursor = node.walk();
    let mut content = String::new();
    for part in node.named_children(&mut cursor) {
        match part.kind() {
            "string_start" | "string_end" => {}
            "string_content" => content.push_str(part.utf8_text(source).ok()?),
            _ => return None,
        }
    }
    Some(content)
}

/// The dotted name of `a.b` or `a.b as c`.
fn dotted(node: Node) -> Node {
    if node.kind() == "aliased_import" {
        node.child_by_field_name("name").unwrap_or(node)
    } else {
        node
    }
}

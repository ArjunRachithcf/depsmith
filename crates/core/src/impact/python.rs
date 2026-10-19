//! The Python import scanner.
use super::imports::{Import, ImportKind, Scan};
use tree_sitter::{Node, Parser};

/// Every module `source`, a Python file, imports.
///
/// Syntax the parser does not know is skipped: imports outside it are still
/// reported, and the region's first line is listed in [`Scan::unparsed`].
pub fn python_imports(source: &str) -> Scan {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .expect("the bundled Python grammar matches tree-sitter");
    let mut scan = Scan::default();
    if let Some(tree) = parser.parse(source, None) {
        visit(tree.root_node(), source.as_bytes(), &mut scan);
    }
    scan
}

fn visit(node: Node, source: &[u8], scan: &mut Scan) {
    let line = node.start_position().row + 1;
    let text = |node: Node| node.utf8_text(source).unwrap_or_default().to_owned();
    if node.is_error() || node.is_missing() {
        scan.unparsed.push(line);
        return;
    }
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
                            plugin.start_position().row + 1,
                            ImportKind::Plugin,
                        ));
                    }
                }
            }
            return;
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit(child, source, scan);
    }
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

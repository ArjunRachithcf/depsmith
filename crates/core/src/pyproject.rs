//! Standard `pyproject.toml` requirements shared by the Python-aware adapters:
//! PEP 508 strings in `[project]` (PEP 621) and `[dependency-groups]`
//! (PEP 735), plus manager-specific lists such as `tool.uv.dev-dependencies`.
use crate::{constraints::Declaration, Error, Result};
use std::path::Path;

/// The parts of a PEP 508 requirement this tool reads or rewrites.
pub(crate) struct Pep508 {
    pub(crate) name: String,
    /// Byte range of the version specifier, excluding surrounding whitespace
    /// and parentheses; `None` for unversioned and direct-URL requirements.
    pub(crate) specifier: Option<std::ops::Range<usize>>,
}

impl Pep508 {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let start = text.len() - text.trim_start().len();
        let name_end = text[start..]
            .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
            .map_or(text.len(), |i| start + i);
        if name_end == start {
            return None;
        }
        let mut position =
            name_end + (text[name_end..].len() - text[name_end..].trim_start().len());
        if text[position..].starts_with('[') {
            position += text[position..].find(']')? + 1;
            position += text[position..].len() - text[position..].trim_start().len();
        }
        let rest = &text[position..];
        let range = if rest.starts_with('@') {
            None
        } else if let Some(inner) = rest.strip_prefix('(') {
            Some(position + 1..position + 1 + inner.find(')')?)
        } else {
            Some(position..position + rest.find(';').unwrap_or(rest.len()))
        };
        let specifier = range.and_then(|r| {
            let raw = &text[r.clone()];
            let begin = r.start + (raw.len() - raw.trim_start().len());
            let end = r.start + raw.trim_end().len();
            (begin < end).then_some(begin..end)
        });
        Some(Self {
            name: text[start..name_end].to_owned(),
            specifier,
        })
    }
}

/// Replace a string value, keeping its surrounding comments/whitespace and its
/// literal (single-quoted) style when the new text allows it.
pub(crate) fn replace_string(value: &mut toml_edit::Value, new: &str) {
    let decor = value.decor().clone();
    let literal = match &*value {
        toml_edit::Value::String(formatted) => formatted
            .as_repr()
            .and_then(|r| r.as_raw().as_str())
            .is_some_and(|raw| raw.starts_with('\'')),
        _ => false,
    };
    *value = if literal && !new.contains(['\'', '\n']) {
        format!("'{new}'").parse().unwrap_or_else(|_| new.into())
    } else {
        new.into()
    };
    *value.decor_mut() = decor;
}

/// Replaces the requirement of the declaration at (location, package, old
/// requirement), or keeps it when `None`.
pub(crate) type Change<'a> = dyn FnMut(&str, &str, &str) -> Option<String> + 'a;

/// Requirement lists as (location, key path): `project.dependencies`, then
/// optional dependencies and dependency groups by name, then each of `extra`
/// (dotted paths such as `tool.uv.dev-dependencies`) that exists. Tables such
/// as `{include-group = ...}` inside a list are not requirements.
fn lists(document: &toml_edit::Item, extra: &[&str]) -> Vec<(String, Vec<String>)> {
    let names = |path: &[&str]| -> Vec<String> {
        let mut item = Some(document);
        for key in path {
            item = item.and_then(|i| i.get(key));
        }
        let mut names: Vec<String> = item
            .and_then(toml_edit::Item::as_table_like)
            .into_iter()
            .flat_map(|t| t.iter())
            .filter(|(_, list)| list.is_array())
            .map(|(name, _)| name.to_owned())
            .collect();
        names.sort();
        names
    };
    let mut output = vec![];
    if document
        .get("project")
        .and_then(|p| p.get("dependencies"))
        .is_some_and(toml_edit::Item::is_array)
    {
        output.push((
            "project.dependencies".to_owned(),
            vec!["project".into(), "dependencies".into()],
        ));
    }
    for name in names(&["project", "optional-dependencies"]) {
        output.push((
            format!("project.optional-dependencies.{name}"),
            vec!["project".into(), "optional-dependencies".into(), name],
        ));
    }
    for name in names(&["dependency-groups"]) {
        output.push((
            format!("dependency-groups.{name}"),
            vec!["dependency-groups".into(), name],
        ));
    }
    for path in extra {
        let keys: Vec<String> = path.split('.').map(str::to_owned).collect();
        let mut item = Some(document);
        for key in &keys {
            item = item.and_then(|i| i.get(key));
        }
        if item.is_some_and(toml_edit::Item::is_array) {
            output.push(((*path).to_owned(), keys));
        }
    }
    output
}

fn array<'a>(document: &'a toml_edit::Item, keys: &[String]) -> Option<&'a toml_edit::Array> {
    let mut item = document;
    for key in keys {
        item = item.get(key)?;
    }
    item.as_array()
}

/// The PEP 508 requirements of the pyproject `text`, located as
/// `<list>[<index>]`; see [`lists`] for the lists read. Direct URLs and
/// unversioned requirements have an empty requirement.
pub(crate) fn declarations(text: &str, file: &Path, extra: &[&str]) -> Result<Vec<Declaration>> {
    let document: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| Error::Invalid(format!("invalid manifest: {e}")))?;
    let root = document.as_item();
    let mut output = vec![];
    for (location, keys) in lists(root, extra) {
        let Some(items) = array(root, &keys) else {
            continue;
        };
        for (index, value) in items.iter().enumerate() {
            let Some(text) = value.as_str() else {
                continue;
            };
            let Some(requirement) = Pep508::parse(text) else {
                continue;
            };
            output.push(Declaration {
                ecosystem: "pypi".into(),
                package: requirement.name,
                requirement: requirement
                    .specifier
                    .map(|range| text[range].to_owned())
                    .unwrap_or_default(),
                file: file.into(),
                location: format!("{location}[{index}]"),
            });
        }
    }
    Ok(output)
}

/// Rewrite version specifiers in the requirement lists of `document` (see
/// [`lists`]). Extras, markers, parentheses and spacing around the specifier
/// are kept.
pub(crate) fn rewrite(document: &mut toml_edit::DocumentMut, change: &mut Change, extra: &[&str]) {
    for (list, keys) in lists(document.as_item(), extra) {
        let mut item = document.as_item_mut();
        for key in &keys {
            item = &mut item[key.as_str()];
        }
        let Some(array) = item.as_array_mut() else {
            continue;
        };
        for (index, value) in array.iter_mut().enumerate() {
            let Some(text) = value.as_str().map(str::to_owned) else {
                continue;
            };
            let Some(Pep508 {
                name,
                specifier: Some(range),
            }) = Pep508::parse(&text)
            else {
                continue;
            };
            let location = format!("{list}[{index}]");
            if let Some(new) = change(&location, &name, &text[range.clone()]) {
                let spliced = format!("{}{new}{}", &text[..range.start], &text[range.end..]);
                replace_string(value, &spliced);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PYPROJECT: &str = r#"[project]
dependencies = ["six==1.15.0", "requests[socks] (>=2,<3) ; python_version > '3'", "direct @ https://example.invalid/d.whl"]
[project.optional-dependencies]
z = ["urllib3<2"]
a = ["idna"]
[dependency-groups]
dev = ["ruff==0.15.22", {include-group = "a"}]
[tool.uv]
dev-dependencies = ["pytest>=8"] # legacy
"#;

    fn declared(extra: &[&str]) -> Vec<(String, String, String)> {
        declarations(PYPROJECT, Path::new("pyproject.toml"), extra)
            .unwrap()
            .into_iter()
            .map(|d| (d.location, d.package, d.requirement))
            .collect()
    }

    #[test]
    fn requirement_lists_are_read_in_a_stable_order() {
        let expected = [
            ("project.dependencies[0]", "six", "==1.15.0"),
            ("project.dependencies[1]", "requests", ">=2,<3"),
            ("project.dependencies[2]", "direct", ""),
            ("project.optional-dependencies.a[0]", "idna", ""),
            ("project.optional-dependencies.z[0]", "urllib3", "<2"),
            ("dependency-groups.dev[0]", "ruff", "==0.15.22"),
        ]
        .map(|(l, p, r)| (l.to_owned(), p.to_owned(), r.to_owned()));
        assert_eq!(declared(&[]), expected);
        let with_uv = declared(&["tool.uv.dev-dependencies"]);
        assert_eq!(
            with_uv.last().unwrap(),
            &(
                "tool.uv.dev-dependencies[0]".to_owned(),
                "pytest".to_owned(),
                ">=8".to_owned()
            )
        );
    }

    #[test]
    fn rewriting_keeps_extras_markers_and_comments() {
        let mut document: toml_edit::DocumentMut = PYPROJECT.parse().unwrap();
        let mut change = |location: &str, package: &str, old: &str| match (location, package, old) {
            ("project.dependencies[1]", "requests", ">=2,<3") => Some(">=2,<4".to_owned()),
            ("tool.uv.dev-dependencies[0]", "pytest", ">=8") => Some(">=9".to_owned()),
            _ => None,
        };
        rewrite(&mut document, &mut change, &["tool.uv.dev-dependencies"]);
        assert_eq!(
            document.to_string(),
            PYPROJECT
                .replace("(>=2,<3)", "(>=2,<4)")
                .replace("pytest>=8", "pytest>=9")
        );
    }
}

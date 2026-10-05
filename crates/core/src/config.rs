//! Repository configuration: the saved target selection and default options
//! in `depsmith.toml`.
use crate::{Error, Result, UpdateOptions};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fs, path::Path};

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
/// Repository configuration read from `depsmith.toml`.
pub struct Config {
    /// Saved target selection, used when no targets are given explicitly.
    pub targets: Vec<String>,
    /// Default options, overridden by explicit CLI or API options.
    pub options: UpdateOptions,
}

/// Read `depsmith.toml` under `root` (if present) and apply `overrides`, a
/// JSON object of [`UpdateOptions`] fields that take precedence.
///
/// # Errors
///
/// Returns [`Error::Invalid`] for an invalid file, unknown fields or options
/// that fail [`UpdateOptions::validate`].
pub fn settings(root: &Path, overrides: &Value) -> Result<Config> {
    let path = root.join("depsmith.toml");
    let mut config: Config = if path.exists() {
        toml::from_str(&fs::read_to_string(path)?)
            .map_err(|e| Error::Invalid(format!("invalid depsmith.toml: {e}")))?
    } else {
        Config::default()
    };
    let mut merged = serde_json::to_value(&config.options).unwrap();
    let overrides = overrides
        .as_object()
        .ok_or_else(|| Error::Invalid("options must be an object".into()))?;
    for (key, value) in overrides {
        merged
            .as_object_mut()
            .unwrap()
            .insert(key.clone(), value.clone());
    }
    config.options = serde_json::from_value(merged)
        .map_err(|e| Error::Invalid(format!("invalid options: {e}")))?;
    config.options.validate()?;
    Ok(config)
}

/// Append the `targets` not yet selected in `depsmith.toml` to its selection,
/// creating the file if needed and keeping everything else, including
/// comments. Returns the targets added; nothing is written when there are
/// none.
///
/// # Errors
///
/// Returns [`Error::Invalid`] when `depsmith.toml` or its `targets` is invalid.
pub fn add_targets(root: &Path, targets: &[String]) -> Result<Vec<String>> {
    let path = root.join("depsmith.toml");
    let text = if path.exists() {
        fs::read_to_string(&path)?
    } else {
        String::new()
    };
    let mut document: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| Error::Invalid(format!("invalid depsmith.toml: {e}")))?;
    if !document.contains_key("targets") {
        document.insert("targets", toml_edit::value(toml_edit::Array::new()));
    }
    let selection = document["targets"]
        .as_array_mut()
        .ok_or_else(|| Error::Invalid("depsmith.toml: targets must be an array".into()))?;
    let mut added = vec![];
    for target in targets {
        if !selection.iter().any(|t| t.as_str() == Some(target)) && !added.contains(target) {
            selection.push(target.as_str());
            added.push(target.clone());
        }
    }
    if !added.is_empty() {
        fs::write(path, document.to_string())?;
    }
    Ok(added)
}

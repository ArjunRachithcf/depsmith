use crate::{Error, Result, UpdateOptions};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fs, path::Path};

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub targets: Vec<String>,
    pub options: UpdateOptions,
}

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

/// Save a target selection as `targets` in `depsmith.toml`, creating the file if
/// needed and keeping its other content and comments. An existing non-empty
/// selection is never replaced.
pub fn save_targets(root: &Path, targets: &[String]) -> Result<()> {
    let path = root.join("depsmith.toml");
    let text = if path.exists() {
        fs::read_to_string(&path)?
    } else {
        String::new()
    };
    let mut document: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| Error::Invalid(format!("invalid depsmith.toml: {e}")))?;
    let existing = document.get("targets").and_then(|t| t.as_array());
    if existing.is_some_and(|t| !t.is_empty()) {
        return Err(Error::Invalid(
            "depsmith.toml already selects targets; edit it to change the selection".into(),
        ));
    }
    let array: toml_edit::Array = targets.iter().map(String::as_str).collect();
    match document.get_mut("targets").and_then(|t| t.as_value_mut()) {
        // Keep the comment/whitespace around an existing empty selection.
        Some(value) => {
            let decor = value.decor().clone();
            *value = array.into();
            *value.decor_mut() = decor;
        }
        None => {
            document.insert("targets", toml_edit::value(array));
        }
    }
    fs::write(path, document.to_string())?;
    Ok(())
}

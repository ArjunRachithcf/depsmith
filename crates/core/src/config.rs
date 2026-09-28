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

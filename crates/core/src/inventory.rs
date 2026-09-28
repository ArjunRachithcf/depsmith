use crate::{Error, Package, Result};
use serde_yaml::Value;

pub fn pixi_inventory(text: &str) -> Result<Vec<Package>> {
    let value: Value = serde_yaml::from_str(text)
        .map_err(|e| Error::Operation(format!("invalid Pixi lockfile: {e}")))?;
    let version = value.get("version").and_then(Value::as_u64).unwrap_or(0);
    if !(5..=7).contains(&version) {
        return Err(Error::Operation(format!(
            "unsupported Pixi lock schema {version}; supported: 5–7"
        )));
    }
    let packages = value
        .get("packages")
        .and_then(Value::as_sequence)
        .ok_or_else(|| Error::Operation("Pixi lockfile has no package list".into()))?;
    let mut platforms =
        std::collections::BTreeMap::<(String, String), std::collections::BTreeSet<String>>::new();
    if let Some(environments) = value.get("environments").and_then(Value::as_mapping) {
        for environment in environments.values() {
            if let Some(targets) = environment.get("packages").and_then(Value::as_mapping) {
                for (platform, records) in targets {
                    if let (Some(platform), Some(records)) =
                        (platform.as_str(), records.as_sequence())
                    {
                        for record in records {
                            for ecosystem in ["conda", "pypi"] {
                                if let Some(artifact) =
                                    record.get(ecosystem).and_then(Value::as_str)
                                {
                                    platforms
                                        .entry((ecosystem.into(), artifact.into()))
                                        .or_default()
                                        .insert(platform.into());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    let mut result = vec![];
    for item in packages {
        let (ecosystem, artifact) = if let Some(a) = item.get("conda").and_then(Value::as_str) {
            ("conda", a)
        } else if let Some(a) = item.get("pypi").and_then(Value::as_str) {
            ("pypi", a)
        } else {
            return Err(Error::Operation("unknown Pixi package record".into()));
        };
        let filename = artifact
            .rsplit('/')
            .next()
            .unwrap_or(artifact)
            .split(['?', '#'])
            .next()
            .unwrap_or(artifact);
        let stem = filename
            .trim_end_matches(".conda")
            .trim_end_matches(".tar.bz2");
        let parts: Vec<_> = stem.rsplitn(3, '-').collect();
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| {
                if ecosystem == "conda" && parts.len() == 3 {
                    Some(parts[2])
                } else {
                    None
                }
            })
            .ok_or_else(|| Error::Operation(format!("missing name for {ecosystem} package")))?;
        let version = item
            .get("version")
            .and_then(Value::as_str)
            .or_else(|| {
                if ecosystem == "conda" && parts.len() == 3 {
                    Some(parts[1])
                } else {
                    None
                }
            })
            .unwrap_or("unknown");
        let platform = item
            .get("subdir")
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                if ecosystem == "conda" {
                    artifact.rsplit('/').nth(1).unwrap_or("unknown")
                } else {
                    "unknown"
                }
            });
        let targets = platforms
            .get(&(ecosystem.into(), artifact.into()))
            .cloned()
            .unwrap_or_else(|| [platform.to_owned()].into_iter().collect());
        for platform in targets {
            result.push(Package {
                ecosystem: ecosystem.into(),
                name: name.into(),
                version: version.into(),
                artifact: artifact.into(),
                platform,
            });
        }
    }
    Ok(result)
}

//! Reading resolved packages and lock graphs from lockfiles.
use crate::{
    graph::{LockGraph, Node, Requirement},
    Error, LockPlatform, Package, Result,
};
use serde_yaml::Value;
use std::collections::{BTreeMap, BTreeSet};

/// One package record of a `pixi.lock`, with the platforms it is locked
/// for, each with the environments that lock it there.
struct Record<'a> {
    ecosystem: &'static str,
    artifact: &'a str,
    name: &'a str,
    version: &'a str,
    item: &'a Value,
    platforms: BTreeMap<String, BTreeSet<String>>,
}

/// The keys of `pixi.lock` package records, with each one's ecosystem:
/// binary conda packages, conda packages built from source, PyPI packages.
const RECORD_KINDS: [(&str, &str); 3] = [
    ("conda", "conda"),
    ("conda_source", "conda"),
    ("pypi", "pypi"),
];

/// Where a record's package comes from: a source package's source, else its
/// artifact.
fn source(artifact: &str) -> &str {
    artifact
        .split_once(" @ ")
        .map_or(artifact, |(_, source)| source)
}

/// The package records of a parsed `pixi.lock`, each with the platforms of
/// the environments that use it and those environments (or its own subdir,
/// with none, when none list it).
fn records(value: &Value) -> Result<Vec<Record<'_>>> {
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
    let mut platforms = BTreeMap::<(String, String), BTreeMap<String, BTreeSet<String>>>::new();
    if let Some(environments) = value.get("environments").and_then(Value::as_mapping) {
        for (name, environment) in environments {
            let name = name.as_str().unwrap_or_default();
            if let Some(targets) = environment.get("packages").and_then(Value::as_mapping) {
                for (platform, records) in targets {
                    if let (Some(platform), Some(records)) =
                        (platform.as_str(), records.as_sequence())
                    {
                        for record in records {
                            for (key, ecosystem) in RECORD_KINDS {
                                if let Some(artifact) = record.get(key).and_then(Value::as_str) {
                                    platforms
                                        .entry((ecosystem.into(), artifact.into()))
                                        .or_default()
                                        .entry(platform.into())
                                        .or_default()
                                        .insert(name.into());
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
        let Some((key, ecosystem, artifact)) = RECORD_KINDS.iter().find_map(|(key, ecosystem)| {
            item.get(key)
                .and_then(Value::as_str)
                .map(|a| (*key, *ecosystem, a))
        }) else {
            return Err(Error::Operation("unknown Pixi package record".into()));
        };
        // A source package (pixi-build) is `NAME[HASH] @ SOURCE`.
        let source_name = (key == "conda_source")
            .then(|| artifact.split(['[', ' ']).next())
            .flatten();
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
            .or(source_name)
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
        let platforms = platforms
            .get(&(ecosystem.into(), artifact.into()))
            .cloned()
            .unwrap_or_else(|| BTreeMap::from([(platform.to_owned(), BTreeSet::new())]));
        result.push(Record {
            ecosystem,
            artifact,
            name,
            version,
            item,
            platforms,
        });
    }
    Ok(result)
}

fn parse(text: &str) -> Result<Value> {
    serde_yaml::from_str(text).map_err(|e| Error::Operation(format!("invalid Pixi lockfile: {e}")))
}

/// The resolved packages of every environment and platform in a `pixi.lock`.
/// Packages whose lock record has no version keep an empty version and are
/// later reported as unassessed.
///
/// # Errors
///
/// Returns [`crate::Error::Operation`] when the lock cannot be parsed or uses
/// an unsupported schema.
pub fn pixi_inventory(text: &str) -> Result<Vec<Package>> {
    let value = parse(text)?;
    let mut result = vec![];
    for record in records(&value)? {
        for (platform, environments) in record.platforms {
            result.push(Package {
                ecosystem: record.ecosystem.into(),
                name: record.name.into(),
                version: record.version.into(),
                artifact: source(record.artifact).into(),
                platform,
                environments: environments.into_iter().collect(),
            });
        }
    }
    Ok(result)
}

/// The named platforms of a `pixi.lock` (schema 7), each with its subdir
/// and sorted virtual packages; none for older schemas, which lock packages
/// under the subdir itself.
///
/// # Errors
///
/// Returns [`crate::Error::Operation`] when the lock cannot be parsed.
pub fn pixi_platforms(text: &str) -> Result<Vec<LockPlatform>> {
    let value = parse(text)?;
    let platforms = value
        .get("platforms")
        .and_then(Value::as_sequence)
        .into_iter()
        .flatten()
        .filter_map(|platform| {
            let name = platform.get("name")?.as_str()?;
            let subdir = platform
                .get("subdir")
                .and_then(Value::as_str)
                .unwrap_or(name);
            let mut virtual_packages: Vec<String> = platform
                .get("virtual-packages")
                .and_then(Value::as_sequence)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            virtual_packages.sort();
            Some(LockPlatform {
                name: name.into(),
                subdir: subdir.into(),
                virtual_packages,
            })
        })
        .collect();
    Ok(platforms)
}

/// The lock graph of a `pixi.lock`: each package on each platform with what
/// it requires. Conda requirements come from `depends`, without virtual
/// packages such as `__cuda`; PyPI ones from `requires_dist`, without those
/// that apply only to an extra.
///
/// # Errors
///
/// Returns [`crate::Error::Operation`] when the lock cannot be parsed or uses
/// an unsupported schema.
pub fn pixi_graph(text: &str) -> Result<LockGraph> {
    let value = parse(text)?;
    let mut nodes = vec![];
    for record in records(&value)? {
        let requires: Vec<Requirement> = match record.ecosystem {
            "conda" => strings(record.item, "depends")
                .filter_map(|line| {
                    let (name, spec) = line.split_once(' ').unwrap_or((line, ""));
                    (!name.starts_with("__")).then(|| requirement(name, spec))
                })
                .collect(),
            _ => strings(record.item, "requires_dist")
                .filter_map(pypi_requirement)
                .collect(),
        };
        for platform in record.platforms.into_keys() {
            nodes.push(Node {
                ecosystem: record.ecosystem.into(),
                name: record.name.into(),
                version: None,
                platform,
                requires: requires.clone(),
            });
        }
    }
    Ok(LockGraph { nodes })
}

/// A `requires_dist` entry (PEP 508) as a requirement, or `None` when it
/// applies only to an extra. Extras in brackets are dropped, a parenthesized
/// version requirement is unwrapped, and a direct URL is not a version.
fn pypi_requirement(line: &str) -> Option<Requirement> {
    let (requirement_text, marker) = line.split_once(';').unwrap_or((line, ""));
    if marker.contains("extra") {
        return None;
    }
    let text = requirement_text.trim();
    let end = text
        .find(|c: char| !(c.is_ascii_alphanumeric() || "._-".contains(c)))
        .unwrap_or(text.len());
    let rest = text[end..].trim_start();
    let rest = match rest.strip_prefix('[') {
        Some(extras) => extras.split_once(']').map_or("", |(_, r)| r),
        None => rest,
    };
    let spec = rest.trim().trim_start_matches('(').trim_end_matches(')');
    let spec = if spec.starts_with('@') { "" } else { spec };
    Some(requirement(&text[..end], spec))
}

/// The string items of the list `key` of a lock record.
fn strings<'a>(item: &'a Value, key: &str) -> impl Iterator<Item = &'a str> {
    item.get(key)
        .and_then(Value::as_sequence)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
}

fn requirement(name: &str, spec: &str) -> Requirement {
    let spec = spec.trim();
    Requirement {
        name: name.trim().into(),
        spec: (!spec.is_empty()).then(|| spec.into()),
        version: None,
    }
}

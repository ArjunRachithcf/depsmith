//! PyPI availability evidence through the PEP 691 JSON Simple API. Index URLs
//! come from the manifest's `[pypi-options]` (Pixi) or its uv settings;
//! credentials are never sent.
use crate::{Error, Result};
use serde_json::Value;

pub(crate) const DEFAULT_INDEX: &str = "https://pypi.org/simple";

/// Parse a URL and drop any `user:password@`, so it is safe to request or show.
fn without_credentials(text: &str) -> Option<reqwest::Url> {
    let mut url = reqwest::Url::parse(text.trim()).ok()?;
    url.set_username("").ok()?;
    url.set_password(None).ok()?;
    Some(url)
}

fn display(url: &reqwest::Url) -> String {
    url.as_str().trim_end_matches('/').to_owned()
}

fn options(table: &toml::Value, primary: &mut Vec<String>, extra: &mut Vec<String>) {
    let urls = |value: Option<&toml::Value>| -> Vec<String> {
        let values = match value {
            Some(toml::Value::String(one)) => vec![one.as_str()],
            Some(toml::Value::Array(many)) => many.iter().filter_map(|v| v.as_str()).collect(),
            _ => vec![],
        };
        values
            .into_iter()
            .filter_map(without_credentials)
            .map(|u| display(&u))
            .collect()
    };
    primary.extend(urls(table.get("index-url")));
    extra.extend(urls(table.get("extra-index-urls")));
}

fn feature_options(value: &toml::Value, primary: &mut Vec<String>, extra: &mut Vec<String>) {
    for (key, val) in value.as_table().into_iter().flatten() {
        if key == "pypi-options" {
            options(val, primary, extra);
        } else {
            feature_options(val, primary, extra);
        }
    }
}

/// `index-url` (replacing the default) and `extra-index-urls` from the manifest
/// and its features, with any embedded credentials removed.
pub(crate) fn index_urls(manifest: &toml::Value) -> Vec<String> {
    let (mut primary, mut extra) = (vec![], vec![]);
    // Workspace-level options first, then features, in manifest key order.
    if let Some(root) = manifest.get("pypi-options") {
        options(root, &mut primary, &mut extra);
    }
    for (key, val) in manifest.as_table().into_iter().flatten() {
        if key != "pypi-options" {
            feature_options(val, &mut primary, &mut extra);
        }
    }
    if primary.is_empty() {
        primary.push(DEFAULT_INDEX.into());
    }
    let mut all: Vec<String> = vec![];
    for url in primary.into_iter().chain(extra) {
        if !all.contains(&url) {
            all.push(url);
        }
    }
    all
}

/// Indexes of uv `settings` (a `uv.toml`, or a pyproject's `[tool.uv]`) in
/// uv's priority order: `[[index]]` entries, then `extra-index-url`, then the
/// default (an index marked `default`, `index-url`, or PyPI). Explicit
/// indexes serve only packages pinned to them and are left out; embedded
/// credentials are removed.
pub(crate) fn uv_index_urls(settings: Option<&toml::Value>) -> Vec<String> {
    let url = |value: &toml::Value| {
        value
            .as_str()
            .and_then(without_credentials)
            .map(|u| display(&u))
    };
    let field = |name: &str| settings.and_then(|s| s.get(name));
    let mut named = vec![];
    let mut default = None;
    for index in field("index")
        .and_then(|i| i.as_array())
        .into_iter()
        .flatten()
    {
        let flag = |name: &str| index.get(name).and_then(|v| v.as_bool()) == Some(true);
        let Some(location) = index.get("url").and_then(url) else {
            continue;
        };
        if flag("default") {
            default = Some(location);
        } else if !flag("explicit") {
            named.push(location);
        }
    }
    let extra: Vec<String> = match field("extra-index-url") {
        Some(toml::Value::Array(many)) => many.iter().filter_map(url).collect(),
        Some(one) => url(one).into_iter().collect(),
        None => vec![],
    };
    let default = default
        .or_else(|| field("index-url").and_then(url))
        .unwrap_or_else(|| DEFAULT_INDEX.into());
    let mut all: Vec<String> = vec![];
    for location in named.into_iter().chain(extra).chain([default]) {
        if !all.contains(&location) {
            all.push(location);
        }
    }
    all
}

/// Fetch a project page from a credential-free index URL.
pub(crate) fn fetch(index: &str, package: &str, timeout: u64) -> Result<Value> {
    const JSON: &str = "application/vnd.pypi.simple.v1+json";
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout))
        .user_agent("depsmith/0.1")
        .build()
        .map_err(|_| Error::Operation("cannot construct HTTP client".into()))?;
    let url = format!("{index}/{}/", crate::adapter::pypi_key(package));
    let response = client
        .get(&url)
        .header(reqwest::header::ACCEPT, JSON)
        .send()
        .map_err(|e| Error::Operation(format!("request failed: {}", e.without_url())))?;
    if !response.status().is_success() {
        return Err(Error::Operation(format!("HTTP {}", response.status())));
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !content_type.starts_with(JSON) {
        return Err(Error::Operation(
            "index does not offer the PEP 691 JSON API".into(),
        ));
    }
    response
        .json()
        .map_err(|e| Error::Operation(format!("unreadable index response: {}", e.without_url())))
}

/// Version from a wheel or sdist filename; other distribution types are skipped.
fn file_version(filename: &str) -> Option<&str> {
    if filename.ends_with(".whl") {
        return filename.split('-').nth(1);
    }
    let stem = filename
        .strip_suffix(".tar.gz")
        .or_else(|| filename.strip_suffix(".zip"))?;
    stem.rsplit_once('-').map(|(_, version)| version)
}

/// Per index, cite the newest final, non-yanked release the requirement
/// excludes. Errors when the requirement is not a valid PEP 440 specifier set.
pub(crate) fn evidence(
    package: &str,
    pages: &[(String, Value)],
    requirement: &str,
    cutoff: Option<i64>,
) -> Result<Vec<crate::constraint::Excluded>> {
    use crate::pep440::{satisfies, Version};
    if satisfies(requirement, &Version::parse("0").unwrap()).is_none() {
        return Err(Error::Operation(format!(
            "unsupported PEP 440 requirement: {requirement}"
        )));
    }
    let mut evidence = vec![];
    for (index, page) in pages {
        let base =
            reqwest::Url::parse(&format!("{index}/{}/", crate::adapter::pypi_key(package))).ok();
        let releases: Vec<(Version, &Value)> = page["files"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|f| matches!(f.get("yanked"), None | Some(Value::Bool(false))))
            // Undated files cannot be shown to predate a release-age cutoff.
            .filter(|f| {
                cutoff.is_none_or(|c| {
                    f["upload-time"]
                        .as_str()
                        .and_then(crate::cutoff::timestamp)
                        .is_some_and(|t| t <= c)
                })
            })
            .filter_map(|f| {
                let version = Version::parse(file_version(f["filename"].as_str()?)?)?;
                (!version.is_prerelease()).then_some((version, f))
            })
            .collect();
        let Some((newest, file)) = releases.iter().max_by(|a, b| a.0.cmp(&b.0)) else {
            continue;
        };
        let allowed = releases
            .iter()
            .filter(|(v, _)| satisfies(requirement, v) == Some(true))
            .map(|(v, _)| v)
            .max();
        if allowed.is_some_and(|a| a >= newest) {
            continue;
        }
        let url = file["url"]
            .as_str()
            .and_then(|u| base.as_ref()?.join(u).ok())
            .and_then(|u| without_credentials(u.as_str()))
            .map_or("unknown artifact".into(), |u| u.to_string());
        evidence.push(crate::constraint::Excluded {
            policy: None,
            scope: index.clone(),
            version: file_version(file["filename"].as_str().unwrap())
                .unwrap()
                .into(),
            allowed: allowed.map(|a| a.text().into()),
            url,
            sha256: file["hashes"]["sha256"]
                .as_str()
                .unwrap_or("unknown")
                .into(),
        });
    }
    Ok(evidence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn file(name: &str, yanked: Value) -> Value {
        json!({"filename": name, "url": format!("https://files.example/{name}"),
               "hashes": {"sha256": format!("sha-{name}")}, "yanked": yanked})
    }

    #[test]
    fn collects_index_urls_without_credentials() {
        let manifest: toml::Value = r#"
[pypi-options]
extra-index-urls = ["https://user:token@private.example/simple/"]
[feature.gpu.pypi-options]
extra-index-urls = ["https://gpu.example/simple"]
"#
        .parse()
        .unwrap();
        assert_eq!(
            index_urls(&manifest),
            [
                DEFAULT_INDEX,
                "https://private.example/simple",
                "https://gpu.example/simple"
            ]
        );
        let replaced: toml::Value = "[pypi-options]\nindex-url = 'https://mirror.example/simple'\n"
            .parse()
            .unwrap();
        assert_eq!(index_urls(&replaced), ["https://mirror.example/simple"]);
    }

    #[test]
    fn cites_newest_final_release_excluded_per_index() {
        let page = json!({"files": [
            file("ruff-0.15.22-py3-none-manylinux_2_17_x86_64.whl", json!(false)),
            file("ruff-0.16.9.tar.gz", json!(false)),
            file("ruff-0.17.0-py3-none-any.whl", json!("broken build")),
            file("ruff-0.18.0rc1-py3-none-any.whl", json!(false)),
            file("ruff-0.16.0.exe", json!(false)),
        ]});
        let relative = json!({"files": [{"filename": "ruff-0.16.1.zip",
            "url": "../../packages/ruff-0.16.1.zip", "hashes": {}}]});
        let pages = [
            (DEFAULT_INDEX.to_owned(), page.clone()),
            ("https://mirror.example/simple".to_owned(), relative),
        ];
        let lines: Vec<String> = evidence("ruff", &pages, "==0.15.22", None)
            .unwrap()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            lines,
            [
                "https://pypi.org/simple: 0.16.9 is excluded (newest allowed 0.15.22): https://files.example/ruff-0.16.9.tar.gz sha256:sha-ruff-0.16.9.tar.gz",
                "https://mirror.example/simple: 0.16.1 is excluded (newest allowed none): https://mirror.example/packages/ruff-0.16.1.zip sha256:unknown",
            ]
        );
        assert!(evidence("ruff", &pages[..1], ">=0.15,<1", None)
            .unwrap()
            .is_empty());
        assert!(evidence("ruff", &pages, "=>1", None).is_err());
        let dated = json!({"files": [
            {"filename": "six-1.15.0.tar.gz", "url": "https://f/a", "hashes": {}, "upload-time": "2020-05-21T08:00:00Z"},
            {"filename": "six-1.16.0.tar.gz", "url": "https://f/b", "hashes": {}, "upload-time": "2021-05-05T14:00:00.123Z"},
            {"filename": "six-1.17.0.tar.gz", "url": "https://f/c", "hashes": {}, "upload-time": "2024-12-04T17:00:00Z"},
            {"filename": "six-1.18.0.tar.gz", "url": "https://f/d", "hashes": {}},
        ]});
        let pages = [(DEFAULT_INDEX.to_owned(), dated)];
        let cutoff = crate::cutoff::timestamp("2022-01-01T00:00:00Z");
        let newest: Vec<String> = evidence("six", &pages, "==1.15.0", cutoff)
            .unwrap()
            .into_iter()
            .map(|e| e.version)
            .collect();
        assert_eq!(newest, ["1.16.0"]);
    }
}

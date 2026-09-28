//! PEP 440 versions and specifiers, sufficient to decide whether a declared
//! requirement excludes the newest final release. Callers exclude pre-releases,
//! so pre-release admission rules for specifiers are not modelled.
use regex::Regex;
use std::{cmp::Ordering, sync::OnceLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Low,
    Value(u64, u64),
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Local {
    Text(String),
    Number(u64),
}

#[derive(Debug, Clone)]
pub(crate) struct Version {
    text: String,
    epoch: u64,
    release: Vec<u64>,
    pre: Option<(u64, u64)>,
    post: Option<u64>,
    dev: Option<u64>,
    local: Vec<Local>,
}

fn pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    // The canonical grammar from the `packaging` reference implementation.
    PATTERN.get_or_init(|| {
        Regex::new(
            r"(?ix)^\s*v?
            (?:(?P<epoch>[0-9]+)!)?
            (?P<release>[0-9]+(?:\.[0-9]+)*)
            (?P<pre>[-_.]?(?P<pre_l>alpha|beta|preview|pre|rc|a|b|c)[-_.]?(?P<pre_n>[0-9]+)?)?
            (?P<post>(?:-(?P<post_n1>[0-9]+))|(?:[-_.]?(?P<post_l>post|rev|r)[-_.]?(?P<post_n2>[0-9]+)?))?
            (?P<dev>[-_.]?dev[-_.]?(?P<dev_n>[0-9]+)?)?
            (?:\+(?P<local>[a-z0-9]+(?:[-_.][a-z0-9]+)*))?
            \s*$",
        )
        .unwrap()
    })
}

impl Version {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let captures = pattern().captures(text)?;
        let number = |name: &str| -> Option<Option<u64>> {
            match captures.name(name) {
                Some(m) => m.as_str().parse().ok().map(Some),
                None => Some(None),
            }
        };
        let release = captures["release"]
            .split('.')
            .map(str::parse)
            .collect::<Result<Vec<u64>, _>>()
            .ok()?;
        let pre = match captures.name("pre_l") {
            Some(label) => {
                let rank = match label.as_str().to_ascii_lowercase().as_str() {
                    "a" | "alpha" => 0,
                    "b" | "beta" => 1,
                    _ => 2,
                };
                Some((rank, number("pre_n")?.unwrap_or(0)))
            }
            None => None,
        };
        let post = if captures.name("post").is_some() {
            Some(number("post_n1")?.or(number("post_n2")?).unwrap_or(0))
        } else {
            None
        };
        let dev = if captures.name("dev").is_some() {
            Some(number("dev_n")?.unwrap_or(0))
        } else {
            None
        };
        let local = captures
            .name("local")
            .map(|m| {
                m.as_str()
                    .split(['-', '_', '.'])
                    .map(|part| match part.parse() {
                        Ok(n) => Local::Number(n),
                        Err(_) => Local::Text(part.to_ascii_lowercase()),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(Self {
            text: text.trim().to_owned(),
            epoch: number("epoch")?.unwrap_or(0),
            release,
            pre,
            post,
            dev,
            local,
        })
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn is_prerelease(&self) -> bool {
        self.pre.is_some() || self.dev.is_some()
    }

    fn release_key(&self) -> &[u64] {
        let end = self
            .release
            .iter()
            .rposition(|&n| n != 0)
            .map_or(0, |i| i + 1);
        &self.release[..end]
    }

    /// Ordering without the local label.
    fn cmp_public(&self, other: &Self) -> Ordering {
        let key = |v: &Self| {
            let pre = match (v.pre, v.post, v.dev) {
                (None, None, Some(_)) => Key::Low,
                (None, _, _) => Key::High,
                (Some((rank, n)), _, _) => Key::Value(rank, n),
            };
            let post = v.post.map_or(Key::Low, |n| Key::Value(0, n));
            let dev = v.dev.map_or(Key::High, |n| Key::Value(0, n));
            (pre, post, dev)
        };
        self.epoch
            .cmp(&other.epoch)
            .then_with(|| self.release_key().cmp(other.release_key()))
            .then_with(|| key(self).cmp(&key(other)))
    }

    /// Release segments of `self` equal `prefix`, padding `self` with zeros.
    fn has_release_prefix(&self, prefix: &[u64]) -> bool {
        prefix
            .iter()
            .enumerate()
            .all(|(i, n)| self.release.get(i).copied().unwrap_or(0) == *n)
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        // No local label sorts first; otherwise segments compare in order.
        self.cmp_public(other)
            .then_with(|| match (self.local.is_empty(), other.local.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Less,
                (false, true) => Ordering::Greater,
                (false, false) => self.local.cmp(&other.local),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Version {}

fn clause(text: &str, version: &Version) -> Option<bool> {
    let text = text.trim();
    let (operator, rest) = ["===", "~=", "==", "!=", "<=", ">=", "<", ">"]
        .iter()
        .find_map(|op| text.strip_prefix(op).map(|rest| (*op, rest.trim())))?;
    if operator == "===" {
        return Some(version.text.eq_ignore_ascii_case(rest));
    }
    if let Some(prefix) = rest.strip_suffix(".*") {
        let prefix = Version::parse(prefix)?;
        if !matches!(operator, "==" | "!=")
            || prefix.pre.is_some()
            || prefix.post.is_some()
            || prefix.dev.is_some()
            || !prefix.local.is_empty()
        {
            return None;
        }
        let matched = version.epoch == prefix.epoch && version.has_release_prefix(&prefix.release);
        return Some(matched == (operator == "=="));
    }
    let spec = Version::parse(rest)?;
    let public = version.cmp_public(&spec);
    Some(match operator {
        "==" | "!=" => {
            let equal = if spec.local.is_empty() {
                public == Ordering::Equal
            } else {
                version == &spec
            };
            equal == (operator == "==")
        }
        "~=" => {
            if spec.release.len() < 2 {
                return None;
            }
            public != Ordering::Less
                && version.epoch == spec.epoch
                && version.has_release_prefix(&spec.release[..spec.release.len() - 1])
        }
        "<=" => public != Ordering::Greater,
        ">=" => public != Ordering::Less,
        "<" => public == Ordering::Less,
        _ => {
            // `>V` excludes post-releases of V unless V is itself a post-release.
            let post_of_spec = spec.post.is_none()
                && version.post.is_some()
                && version.epoch == spec.epoch
                && version.release_key() == spec.release_key();
            public == Ordering::Greater && !post_of_spec
        }
    })
}

/// Whether `version` satisfies a comma-separated specifier set; `None` when the
/// specifier cannot be parsed.
pub(crate) fn satisfies(specifier: &str, version: &Version) -> Option<bool> {
    let mut result = true;
    for part in specifier
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty() && *p != "*")
    {
        result &= clause(part, version)?;
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::{satisfies, Version};

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap_or_else(|| panic!("unparseable {text}"))
    }

    #[test]
    fn orders_versions_per_pep_440() {
        // Ascending, from PEP 440's summary of permitted suffixes and ordering.
        let ascending = [
            "1.dev0",
            "1.0.dev456",
            "1.0a1",
            "1.0a2.dev456",
            "1.0a12.dev456",
            "1.0a12",
            "1.0b1.dev456",
            "1.0b2",
            "1.0b2.post345.dev456",
            "1.0b2.post345",
            "1.0rc1.dev456",
            "1.0rc1",
            "1.0",
            "1.0+abc.5",
            "1.0+abc.7",
            "1.0+5",
            "1.0.post456.dev34",
            "1.0.post456",
            "1.0.15",
            "1.1.dev1",
            "2!0.1",
        ];
        for pair in ascending.windows(2) {
            assert!(v(pair[0]) < v(pair[1]), "{} < {}", pair[0], pair[1]);
        }
        assert_eq!(v("1.0"), v("1.0.0"));
        assert_eq!(v("V1.0-ALPHA.1"), v("1.0a1"));
        assert_eq!(v("1.0-1"), v("1.0.post1"));
        assert_eq!(v("1.0c1"), v("1.0rc1"));
        assert!(v("1.0rc1").is_prerelease());
        assert!(v("1.0.dev1").is_prerelease());
        assert!(!v("1.0.post1").is_prerelease());
        assert!(Version::parse("not-a-version").is_none());
    }

    #[test]
    fn matches_specifiers() {
        let cases = [
            ("==0.15.22", "0.15.22", true),
            ("==0.15.22", "0.15.23", false),
            ("==1.0", "1.0.0", true),
            ("==1.0", "1.0+local", true),
            ("==1.0+local", "1.0", false),
            ("==1.*", "1.9.3", true),
            ("==1.*", "2.0", false),
            ("!=1.*", "2.0", true),
            ("~=1.4", "1.9", true),
            ("~=1.4", "2.0", false),
            ("~=1.4.5", "1.4.9", true),
            ("~=1.4.5", "1.5.0", false),
            (">=3.8,<4", "3.12", true),
            (">=3.8,<4", "4.0", false),
            ("<=12.9.0", "12.9.0", true),
            ("<=12.9.0", "12.9.2", false),
            (">1.7", "1.7.post1", false),
            (">1.7", "1.7.1", true),
            ("===1.0", "1.0", true),
            ("===1.0", "1.0.0", false),
            ("<2", "2.0.post1", false),
        ];
        for (specifier, version, expected) in cases {
            assert_eq!(
                satisfies(specifier, &v(version)),
                Some(expected),
                "{version} {specifier}"
            );
        }
        assert_eq!(satisfies("=>1", &v("1.0")), None);
        assert_eq!(satisfies("~=1", &v("1.0")), None);
    }
}

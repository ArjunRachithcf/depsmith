//! Accepting a constraint suggestion: `NAME` restyles the declared requirement
//! to an evidenced newer version; `NAME=REQUIREMENT` replaces it explicitly.
use crate::{Error, Result};

/// A newer release a declared requirement excludes, in one conda subdir or
/// PyPI index. Displays as the evidence line reported to users.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Excluded {
    /// The native release-age policy applied to both sides, e.g. `exclude-newer 14d`.
    pub policy: Option<String>,
    pub scope: String,
    pub version: String,
    pub allowed: Option<String>,
    pub url: String,
    pub sha256: String,
}

impl std::fmt::Display for Excluded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let policy = self
            .policy
            .as_ref()
            .map(|p| format!("; {p} applied"))
            .unwrap_or_default();
        write!(
            f,
            "{}: {} is excluded (newest allowed {}{policy}): {} sha256:{}",
            self.scope,
            self.version,
            self.allowed.as_deref().unwrap_or("none"),
            self.url,
            self.sha256
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Acceptance {
    pub name: String,
    pub requirement: Option<String>,
}

/// `NAME` or `NAME=REQUIREMENT`; the first `=` after the name separates them,
/// so `six===1.17.0` requests `==1.17.0`.
pub(crate) fn parse_accept(text: &str) -> Result<Acceptance> {
    let text = text.trim();
    let end = text
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/')))
        .unwrap_or(text.len());
    let (name, rest) = text.split_at(end);
    let invalid = || {
        Error::Invalid(format!(
            "--accept expects NAME or NAME=REQUIREMENT, got {text:?}"
        ))
    };
    if name.is_empty() {
        return Err(invalid());
    }
    let requirement = match rest.strip_prefix('=') {
        None if rest.is_empty() => None,
        Some(requirement) if !requirement.trim().is_empty() => Some(requirement.trim().to_owned()),
        _ => return Err(invalid()),
    };
    Ok(Acceptance {
        name: name.to_owned(),
        requirement,
    })
}

/// The requirement rewritten to admit `newest` in the same style, or `None`
/// when the transformation is ambiguous and needs an explicit replacement.
pub(crate) fn restyle(requirement: &str, newest: &str) -> Option<String> {
    let requirement = requirement.trim();
    if requirement.contains([',', '|']) {
        return None;
    }
    // `newest` cut or zero-padded to the precision of the declared version.
    let precision = |declared: &str| {
        let parts: Vec<&str> = newest.split('.').collect();
        (0..declared.split('.').count())
            .map(|i| parts.get(i).copied().unwrap_or("0"))
            .collect::<Vec<_>>()
            .join(".")
    };
    let restyled = if requirement.starts_with("===") {
        format!("==={newest}")
    } else if let Some(version) = requirement.strip_prefix("==") {
        match version.strip_suffix(".*") {
            Some(prefix) => format!("=={}.*", precision(prefix)),
            None if version.contains('*') => return None,
            None => format!("=={newest}"),
        }
    } else if let Some(version) = requirement.strip_prefix("~=") {
        format!("~={}", precision(version))
    } else if requirement.starts_with("<=") {
        format!("<={newest}")
    } else if let Some(version) = requirement.strip_prefix('=') {
        if version.contains('*') {
            return None;
        }
        format!("={}", precision(version))
    } else if requirement.starts_with(|c: char| c.is_ascii_digit()) {
        match requirement.strip_suffix(".*") {
            Some(prefix) => format!("{}.*", precision(prefix)),
            None if requirement.contains('*') => return None,
            None => newest.to_owned(),
        }
    } else {
        return None;
    };
    (restyled != requirement).then_some(restyled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_name_and_optional_explicit_requirement() {
        let accept = |name: &str, requirement: Option<&str>| Acceptance {
            name: name.into(),
            requirement: requirement.map(Into::into),
        };
        assert_eq!(parse_accept("ruff").unwrap(), accept("ruff", None));
        assert_eq!(
            parse_accept(" cuda-python=<=13.4.3 ").unwrap(),
            accept("cuda-python", Some("<=13.4.3"))
        );
        assert_eq!(
            parse_accept("sysroot_linux-64==2.39").unwrap(),
            accept("sysroot_linux-64", Some("=2.39"))
        );
        for invalid in ["", "=1.0", "ruff=", ">=1", "ruff >=1"] {
            assert!(parse_accept(invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn preserves_unambiguous_styles() {
        let cases = [
            ("==0.15.22", "0.16.9", Some("==0.16.9")),
            ("===5.1.0", "8.0.1", Some("===8.0.1")),
            ("<=12.9.0", "13.4.3", Some("<=13.4.3")),
            ("<=2.17", "2.39", Some("<=2.39")),
            ("2.17", "2.39", Some("2.39")),
            ("1.2.*", "1.4.5", Some("1.4.*")),
            ("==1.*", "3.0.1", Some("==3.*")),
            ("=1.2", "1.4.5", Some("=1.4")),
            ("~=1.4", "2.1.0", Some("~=2.1")),
            ("~=1.4.2", "1.6", Some("~=1.6.0")),
            // Ambiguous: exclusive bounds, ranges, exclusions and alternatives.
            ("<2", "2.3", None),
            (">=1,<2", "2.3", None),
            ("!=1.0", "2.0", None),
            ("1.0|2.*", "3.0", None),
            ("1.2*", "1.4", None),
            // Never produce a requirement that is unchanged.
            ("==2.0", "2.0", None),
        ];
        for (requirement, newest, expected) in cases {
            assert_eq!(
                restyle(requirement, newest).as_deref(),
                expected,
                "{requirement} -> {newest}"
            );
        }
    }
}

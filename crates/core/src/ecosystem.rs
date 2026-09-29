//! Ecosystems: the namespaces package identities belong to (conda, PyPI,
//! GitHub Actions, ...). Their rules are shared by every package manager that
//! resolves them: a version scheme for ordering versions and restyling
//! constraints, and how a resolved package maps to an upstream identity for
//! vulnerability scanning.
use crate::{Error, Package, Result};
use std::cmp::Ordering;

/// An ecosystem's rules for ordering versions and reading constraints.
pub(crate) trait VersionScheme: Sync {
    /// Order two versions.
    fn compare(&self, left: &str, right: &str) -> Ordering;
    /// Whether the requirement can exclude a newer release: a pin or upper
    /// bound on every `|` alternative, rather than only lower bounds.
    fn caps_newer(&self, requirement: &str) -> bool {
        caps_newer_releases(requirement)
    }
    /// The requirement rewritten to admit `newest` in the same style, or
    /// `None` when that is ambiguous.
    fn restyle(&self, requirement: &str, newest: &str) -> Option<String> {
        crate::constraint::restyle(requirement, newest)
    }
    /// Reject an explicit replacement requirement this ecosystem cannot read.
    fn check_explicit(&self, _name: &str, _requirement: &str) -> Result<()> {
        Ok(())
    }
}

/// Whether a conda or PEP 440 requirement can exclude a newer release. Every `|`
/// alternative needs a `,` clause other than a lower bound or exclusion.
pub(crate) fn caps_newer_releases(requirement: &str) -> bool {
    requirement.split('|').all(|alternative| {
        alternative.split(',').map(str::trim).any(|clause| {
            !(clause.is_empty()
                || clause == "*"
                || clause.starts_with('>')
                || clause.starts_with("!="))
        })
    })
}

struct Conda;

impl VersionScheme for Conda {
    fn compare(&self, left: &str, right: &str) -> Ordering {
        crate::conda_version::compare(left, right)
    }
}

struct Pypi;

impl VersionScheme for Pypi {
    fn compare(&self, left: &str, right: &str) -> Ordering {
        use crate::pep440::Version;
        Version::parse(left).cmp(&Version::parse(right))
    }
    fn check_explicit(&self, name: &str, requirement: &str) -> Result<()> {
        use crate::pep440::{satisfies, Version};
        if satisfies(requirement, &Version::parse("0").unwrap()).is_none() {
            return Err(Error::Invalid(format!(
                "{name}={requirement} is not a PEP 440 requirement; the first `=` separates the name, so pin exactly with --accept {name}===VERSION"
            )));
        }
        Ok(())
    }
}

/// The version scheme of `ecosystem`, if depsmith knows it.
pub(crate) fn scheme(ecosystem: &str) -> Option<&'static dyn VersionScheme> {
    match ecosystem {
        "conda" => Some(&Conda),
        "pypi" => Some(&Pypi),
        _ => None,
    }
}

/// The upstream identity (unversioned PURL, evidence) a resolved package
/// carries by itself: a PyPI artifact on files.pythonhosted.org or a GitHub
/// Actions exact release tag. Anything else needs a reviewed mapping; names
/// alone never imply identity across ecosystems.
pub(crate) fn native_identity(package: &Package) -> Option<(String, String)> {
    match package.ecosystem.as_str() {
        "pypi" => {
            let url = reqwest::Url::parse(&package.artifact).ok()?;
            if url.scheme() != "https" || url.host_str() != Some("files.pythonhosted.org") {
                return None;
            }
            let name = regex::Regex::new("[-_.]+")
                .unwrap()
                .replace_all(&package.name.to_ascii_lowercase(), "-")
                .into_owned();
            Some((
                format!("pkg:pypi/{name}"),
                "native lockfile PyPI identity".into(),
            ))
        }
        "github-actions" => {
            let exact_tag = regex::Regex::new(r"^v?[0-9]+\.[0-9]+\.[0-9]+$").unwrap();
            (package
                .artifact
                .starts_with(&format!("https://github.com/{}@", package.name))
                && exact_tag.is_match(&package.version))
            .then(|| {
                (
                    format!("pkg:github/{}", package.name),
                    "GitHub workflow uses repository and exact release tag".into(),
                )
            })
        }
        _ => None,
    }
}

/// Why an upstream advisory may not apply to this ecosystem's builds, or
/// `None` when its packages are the upstream artifacts.
pub(crate) fn build_caveat(ecosystem: &str) -> Option<&'static str> {
    (ecosystem == "conda")
        .then_some("unknown: upstream advisory does not establish conda build/backport status")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemes_order_versions_by_their_own_rules() {
        let pypi = scheme("pypi").unwrap();
        assert_eq!(pypi.compare("1.10", "1.9"), Ordering::Greater);
        assert_eq!(pypi.compare("1.0rc1", "1.0"), Ordering::Less);
        let conda = scheme("conda").unwrap();
        assert_eq!(conda.compare("1.1.1w", "1.1.1v"), Ordering::Greater);
        assert!(scheme("unknown").is_none());
    }

    #[test]
    fn only_pins_and_upper_bounds_cap_newer_releases() {
        let pypi = scheme("pypi").unwrap();
        for capped in ["==1.0", "<2", "~=1.4", ">=1,<2", "1.0|2.*"] {
            assert!(pypi.caps_newer(capped), "{capped}");
        }
        for open in [">=1", "*", "!=1.2", "1.0|>=2"] {
            assert!(!pypi.caps_newer(open), "{open}");
        }
    }

    #[test]
    fn explicit_pypi_requirements_must_be_pep_440() {
        let pypi = scheme("pypi").unwrap();
        assert!(pypi.check_explicit("six", "==1.17.0").is_ok());
        assert!(pypi.check_explicit("six", "1.17").is_err());
        assert!(scheme("conda")
            .unwrap()
            .check_explicit("ruff", "0.16.*")
            .is_ok());
    }

    #[test]
    fn only_self_evident_packages_have_a_native_identity() {
        let package = |ecosystem: &str, name: &str, version: &str, artifact: &str| Package {
            ecosystem: ecosystem.into(),
            name: name.into(),
            version: version.into(),
            artifact: artifact.into(),
            platform: "linux-64".into(),
        };
        let wheel = "https://files.pythonhosted.org/packages/x/zope.interface-7.0.whl";
        assert_eq!(
            native_identity(&package("pypi", "Zope_Interface", "7.0", wheel))
                .unwrap()
                .0,
            "pkg:pypi/zope-interface"
        );
        let mirror = "https://mirror.invalid/zope.interface-7.0.whl";
        assert!(native_identity(&package("pypi", "zope.interface", "7.0", mirror)).is_none());
        let action = "https://github.com/actions/checkout@v4.2.2";
        assert_eq!(
            native_identity(&package(
                "github-actions",
                "actions/checkout",
                "v4.2.2",
                action
            ))
            .unwrap()
            .0,
            "pkg:github/actions/checkout"
        );
        assert!(
            native_identity(&package("github-actions", "actions/checkout", "v4", action)).is_none()
        );
        assert!(native_identity(&package("conda", "openssl", "3.0", wheel)).is_none());
        assert!(build_caveat("conda").is_some());
        assert!(build_caveat("pypi").is_none());
    }
}

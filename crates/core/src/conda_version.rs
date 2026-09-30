//! Conda version ordering (`VersionOrder`). Matching requirements stays with the
//! native backend; this module only decides which of two versions is newer.
use std::cmp::Ordering;

#[derive(Debug, PartialEq, Eq)]
enum Part {
    Dev,
    Text(String),
    /// Digits without leading zeros, so length then text orders by magnitude.
    Number(String),
    Post,
}

impl Part {
    fn rank(&self) -> u8 {
        match self {
            Part::Dev => 0,
            Part::Text(_) => 1,
            Part::Number(_) => 2,
            Part::Post => 3,
        }
    }
}

impl Ord for Part {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Part::Text(a), Part::Text(b)) => a.cmp(b),
            (Part::Number(a), Part::Number(b)) => a.len().cmp(&b.len()).then_with(|| a.cmp(b)),
            _ => self.rank().cmp(&other.rank()),
        }
    }
}

impl PartialOrd for Part {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn number(digits: &str) -> Part {
    Part::Number(digits.trim_start_matches('0').into())
}

fn components(text: &str) -> Vec<Vec<Part>> {
    // A trailing underscore marks openssl-style patch releases (1.1_ < 1.1a1).
    let (text, patched) = match text.strip_suffix('_') {
        Some(rest) => (rest, true),
        None => (text, false),
    };
    let text = if text.contains('-') && !text.contains('_') {
        text.replace('-', ".")
    } else {
        text.replace('_', ".")
    };
    let mut result: Vec<Vec<Part>> = text
        .split('.')
        .map(|component| {
            let mut parts = vec![];
            let mut rest = component;
            while let Some(first) = rest.chars().next() {
                let digit = first.is_ascii_digit();
                let end = rest
                    .find(|c: char| c.is_ascii_digit() != digit)
                    .unwrap_or(rest.len());
                let (run, tail) = rest.split_at(end);
                parts.push(match run {
                    _ if digit => number(run),
                    "dev" => Part::Dev,
                    "post" => Part::Post,
                    _ => Part::Text(run.into()),
                });
                rest = tail;
            }
            if parts.first().is_some_and(|p| !matches!(p, Part::Number(_))) {
                parts.insert(0, number("0"));
            }
            parts
        })
        .collect();
    if patched {
        if let Some(last) = result.last_mut() {
            last.push(Part::Text("_".into()));
        }
    }
    result
}

/// Missing trailing components and parts compare as zero (1.1 == 1.1.0).
fn compare_padded(left: &[Vec<Part>], right: &[Vec<Part>]) -> Ordering {
    let zero = [number("0")];
    for index in 0..left.len().max(right.len()) {
        let a = left.get(index).map_or(&zero[..], Vec::as_slice);
        let b = right.get(index).map_or(&zero[..], Vec::as_slice);
        for part in 0..a.len().max(b.len()) {
            let ordering = a
                .get(part)
                .unwrap_or(&zero[0])
                .cmp(b.get(part).unwrap_or(&zero[0]));
            if ordering != Ordering::Equal {
                return ordering;
            }
        }
    }
    Ordering::Equal
}

fn parse(version: &str) -> (Part, Vec<Vec<Part>>, Vec<Vec<Part>>) {
    let version = version.trim().to_lowercase();
    let (epoch, rest) = version.split_once('!').unwrap_or(("0", &version));
    let (main, local) = rest.split_once('+').unwrap_or((rest, ""));
    let local = if local.is_empty() {
        vec![]
    } else {
        components(local)
    };
    (number(epoch), components(main), local)
}

pub(crate) fn compare(left: &str, right: &str) -> Ordering {
    let (left_epoch, left_main, left_local) = parse(left);
    let (right_epoch, right_main, right_local) = parse(right);
    left_epoch
        .cmp(&right_epoch)
        .then_with(|| compare_padded(&left_main, &right_main))
        .then_with(|| compare_padded(&left_local, &right_local))
}

/// Whether a conda version carries a conventional pre-release marker.
/// Letters alone are not enough (openssl's `1.1.1w` is a final release).
pub(crate) fn is_prerelease(version: &str) -> bool {
    static MARKER: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    MARKER
        .get_or_init(|| {
            regex::Regex::new(
                r"(?i)(?:^|[0-9._-])(?:alpha|beta|preview|pre|dev|rc|a|b|c)(?:[0-9._-]|$)",
            )
            .unwrap()
        })
        .is_match(version)
}

/// Whether `version` has `prefix`'s components as its leading components.
fn starts_with(version: &str, prefix: &str) -> bool {
    let prefix = prefix.trim_end_matches(['.', '*']);
    version == prefix
        || version.starts_with(&format!("{prefix}."))
        || compare(version, prefix) == Ordering::Equal
}

/// Whether `version` satisfies one clause of a conda version specification.
fn clause(spec: &str, version: &str) -> Option<bool> {
    if spec.is_empty() || spec == "*" {
        return Some(true);
    }
    let (operator, operand) = ["==", "!=", ">=", "<=", "~=", ">", "<", "="]
        .iter()
        .find_map(|op| Some((*op, spec.strip_prefix(op)?.trim())))
        .unwrap_or(("", spec));
    if operand.is_empty()
        || operand
            .chars()
            .any(|c| c.is_whitespace() || "^$()[]|,".contains(c))
    {
        return None;
    }
    let glob = operand.ends_with('*');
    if glob && !matches!(operator, "" | "==" | "!=" | "=") {
        return None;
    }
    let order = compare(version, operand);
    Some(match operator {
        "" | "==" if glob => starts_with(version, operand),
        "" | "==" => order == Ordering::Equal,
        "!=" if glob => !starts_with(version, operand),
        "!=" => order != Ordering::Equal,
        ">=" => order != Ordering::Less,
        "<=" => order != Ordering::Greater,
        ">" => order == Ordering::Greater,
        "<" => order == Ordering::Less,
        "~=" => {
            let parent = operand.rsplit_once('.')?.0;
            order != Ordering::Less && starts_with(version, parent)
        }
        // `=1.2` is fuzzy: 1.2 and any 1.2.x.
        _ => starts_with(version, operand),
    })
}

/// Whether `version` satisfies a conda version specification (`,` binds
/// tighter than `|`). `None` for forms this does not evaluate, such as
/// regular expressions or build strings, so callers never guess.
pub(crate) fn matches(spec: &str, version: &str) -> Option<bool> {
    let mut any = false;
    for alternative in spec.trim().split('|') {
        let mut all = true;
        for part in alternative.split(',') {
            all &= clause(part.trim(), version)?;
        }
        any |= all;
    }
    Some(any)
}

#[cfg(test)]
mod tests {
    use super::{compare, is_prerelease};

    #[test]
    fn recognises_conventional_prerelease_markers_only() {
        for pre in [
            "3.15.0rc2",
            "2.0.0a1",
            "1.0b3",
            "4.0.0.dev20240101",
            "1.0alpha",
            "2.1.0beta2",
            "1.0.pre1",
            "0.9c1",
            "6.0.0.RC1",
        ] {
            assert!(is_prerelease(pre), "{pre}");
        }
        for final_release in [
            "3.12.14",
            "1.1.1w",
            "2.39",
            "1.0.post1",
            "2024.8.30",
            "1.0_1",
            "9e",
            "1.2.3.4",
        ] {
            assert!(!is_prerelease(final_release), "{final_release}");
        }
    }
    use std::cmp::Ordering::{Equal, Less};

    #[test]
    fn follows_documented_conda_version_order() {
        // From conda's VersionOrder documentation, ascending.
        let ordered = [
            ("0.4", Equal, "0.4.0"),
            ("0.4.0", Less, "0.4.1.rc"),
            ("0.4.1.rc", Equal, "0.4.1.RC"),
            ("0.4.1.RC", Less, "0.4.1"),
            ("0.4.1", Less, "0.5a1"),
            ("0.5a1", Less, "0.5b3"),
            ("0.5b3", Less, "0.5C1"),
            ("0.5C1", Less, "0.5"),
            ("0.5", Less, "0.9.6"),
            ("0.9.6", Less, "0.960923"),
            ("0.960923", Less, "1.0"),
            ("1.0", Less, "1.1dev1"),
            ("1.1dev1", Less, "1.1_"),
            ("1.1_", Less, "1.1a1"),
            ("1.1a1", Less, "1.1.0dev1"),
            ("1.1.0dev1", Equal, "1.1.dev1"),
            ("1.1.dev1", Less, "1.1.a1"),
            ("1.1.a1", Less, "1.1.0rc1"),
            ("1.1.0rc1", Less, "1.1.0"),
            ("1.1.0", Equal, "1.1"),
            ("1.1", Less, "1.1.0post1"),
            ("1.1.0post1", Equal, "1.1.post1"),
            ("1.1.post1", Less, "1.1post1"),
            ("1.1post1", Less, "1996.07.12"),
            ("1996.07.12", Less, "1!0.4.1"),
            ("1!0.4.1", Less, "1!3.1.1.6"),
            ("1!3.1.1.6", Less, "2!0.4.1"),
        ];
        for (left, expected, right) in ordered {
            assert_eq!(compare(left, right), expected, "{left} vs {right}");
            assert_eq!(
                compare(right, left),
                expected.reverse(),
                "{right} vs {left}"
            );
        }
    }

    #[test]
    fn local_versions_and_large_numbers() {
        assert_eq!(compare("1.0+2", "1.0+10"), Less);
        assert_eq!(compare("1.0", "1.0+1"), Less);
        assert_eq!(compare("0.15.22", "0.16.9"), Less);
        assert_eq!(compare("2.17", "2.39"), Less);
        assert_eq!(compare("12.9.0", "12.9.2"), Less);
        assert_eq!(compare("20240101", "99999999999999999999"), Less);
    }

    #[test]
    fn specifications_match_like_conda() {
        for (spec, version, expected) in [
            ("=3.12", "3.12.4", Some(true)),
            ("=3.12", "3.13.0", Some(false)),
            ("3.12.*", "3.12.0", Some(true)),
            ("==1.16.0", "1.16", Some(true)),
            ("==1.16.0", "1.16.1", Some(false)),
            (">=1.26,<2", "1.26.4", Some(true)),
            (">=1.26,<2", "2.0.0", Some(false)),
            ("1.0|>=2", "2.1", Some(true)),
            ("~=1.4.2", "1.4.9", Some(true)),
            ("~=1.4.2", "1.5.0", Some(false)),
            ("!=1.2.*", "1.2.3", Some(false)),
            ("*", "0.1", Some(true)),
            ("1.2 py*", "1.2", None),
            ("^1\\.2$", "1.2", None),
        ] {
            assert_eq!(super::matches(spec, version), expected, "{spec} {version}");
        }
    }
}

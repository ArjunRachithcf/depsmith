//! A lock can hold several versions of one package (Python forks, npm
//! nesting, cargo majors): only versions that moved are paired, each once,
//! by shared environments, then within a release line, then newest with
//! newest.
use depsmith_core::{
    adapter::{Adapter, AdapterSpec, Candidate, Capabilities, ManagedFiles},
    Engine, Package, Result, Target, UpdateOptions,
};
use std::{collections::BTreeMap, fs, path::Path};

mod support;

/// Resolves `multi.json` to fixed inventories, one `[ecosystem:]name version
/// [environment,...]` per entry; the ecosystem defaults to pypi.
struct Multi {
    before: Vec<&'static str>,
    after: Vec<&'static str>,
}

fn packages(entries: &[&str]) -> Vec<Package> {
    entries
        .iter()
        .map(|entry| {
            let mut fields = entry.split(' ');
            let (ecosystem, name) = match fields.next().unwrap().split_once(':') {
                Some((ecosystem, name)) => (ecosystem, name),
                None => ("pypi", entry.split(' ').next().unwrap()),
            };
            let version = fields.next().unwrap();
            let environments = fields
                .next()
                .map_or(vec![], |e| e.split(',').map(Into::into).collect());
            Package {
                ecosystem: ecosystem.into(),
                name: name.into(),
                version: version.into(),
                artifact: format!(
                    "https://files.pythonhosted.org/packages/{name}-{version}.tar.gz"
                ),
                platform: "any".into(),
                environments,
            }
        })
        .collect()
}

impl Adapter for Multi {
    fn spec(&self) -> AdapterSpec {
        AdapterSpec {
            manager: "multi".into(),
            ecosystems: vec!["pypi".into(), "conda".into()],
            patterns: vec!["multi.json".into()],
            managed: vec![ManagedFiles {
                manifests: vec!["multi.json".into()],
                inputs: vec!["multi.lock".into()],
            }],
            skip_dirs: vec![],
            tools: vec![],
            capabilities: Capabilities::undeclared(),
        }
    }
    fn detects(&self, _: &Path, _: &str) -> bool {
        true
    }
    fn prepare(&self, stage: &Path, target: &Target, _: &UpdateOptions) -> Result<Candidate> {
        let lock = target.manifest.with_file_name("multi.lock");
        fs::write(stage.join(&lock), "resolved\n")?;
        Ok(Candidate {
            files: vec![lock],
            before: packages(&self.before),
            baseline_available: true,
            after: packages(&self.after),
            ..Default::default()
        })
    }
}

/// The proposal's dependency changes as `before -> after` lines, and the
/// downgrades that held the target back as `held NAME OLD -> NAME NEW`.
fn moved(before: Vec<&'static str>, after: Vec<&'static str>) -> Vec<String> {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("multi.json"), "{}").unwrap();
    fs::write(root.path().join("multi.lock"), "old\n").unwrap();
    let engine = Engine::new(vec![Box::new(Multi { before, after })]);
    let proposal = engine
        .prepare(
            root.path(),
            &["multi:multi.json".into()],
            UpdateOptions::default(),
        )
        .unwrap();
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    let side = |p: &Option<Package>| {
        p.as_ref()
            .map_or("-".to_owned(), |p| format!("{} {}", p.name, p.version))
    };
    let mut lines: Vec<String> = proposal
        .dependencies
        .iter()
        .map(|d| format!("{} -> {}", side(&d.before), side(&d.after)))
        .collect();
    for held in &proposal.unresolved {
        let (old, new) = held.reference.split_once(" -> ").unwrap();
        let old = old.strip_prefix(&format!("{} ", held.package)).unwrap();
        lines.push(format!("held {0} {old} -> {0} {new}", held.package));
    }
    lines.sort();
    lines
}

#[test]
fn an_unchanged_version_is_never_paired_with_a_moved_one() {
    // numpy forked by Python version: 2.2.6 stays, 2.3.5 moves to 2.5.4.
    assert_eq!(
        moved(
            vec!["numpy 2.2.6", "numpy 2.3.5"],
            vec!["numpy 2.2.6", "numpy 2.5.4"]
        ),
        ["numpy 2.3.5 -> numpy 2.5.4"]
    );
}

#[test]
fn moved_versions_pair_newest_with_newest_and_the_rest_are_added_or_removed() {
    assert_eq!(
        moved(vec!["a 1.0", "a 2.0"], vec!["a 1.1", "a 2.1"]),
        ["a 1.0 -> a 1.1", "a 2.0 -> a 2.1"]
    );
    assert_eq!(
        moved(vec!["a 1.0", "a 2.0"], vec!["a 2.1"]),
        ["a 1.0 -> -", "a 2.0 -> a 2.1"]
    );
    assert_eq!(
        moved(vec!["a 2.0"], vec!["a 1.5", "a 2.1"]),
        ["- -> a 1.5", "a 2.0 -> a 2.1"]
    );
}

#[test]
fn versions_pair_within_the_environments_that_lock_them() {
    // Each environment moves its own numpy; newest with newest would pair
    // 2.3.5 with 2.4.0.
    assert_eq!(
        moved(
            vec!["numpy 2.3.5 a", "numpy 2.2.6 b"],
            vec!["numpy 2.3.7 a", "numpy 2.4.0 b"]
        ),
        ["numpy 2.2.6 -> numpy 2.4.0", "numpy 2.3.5 -> numpy 2.3.7"]
    );
    // Environment a moves; environment b newly adds a version.
    assert_eq!(
        moved(vec!["a 2.0 a"], vec!["a 2.1 a", "a 2.5 b"]),
        ["- -> a 2.5", "a 2.0 -> a 2.1"]
    );
}

#[test]
fn conda_and_pypi_entries_of_one_name_pair_separately() {
    assert_eq!(
        moved(
            vec!["conda:numpy 1.26.4", "numpy 2.0.0", "conda:python 3.12.1"],
            vec!["conda:numpy 2.1.0", "numpy 2.0.1", "conda:python 3.12.1"]
        ),
        ["numpy 1.26.4 -> numpy 2.1.0", "numpy 2.0.0 -> numpy 2.0.1"]
    );
}

#[test]
fn moved_versions_pair_within_their_release_line_first() {
    // Two copies (npm nesting, cargo majors): the 3.x copy moves, the 9.x
    // copy goes away; 9.0.5 did not become 3.1.5.
    assert_eq!(
        moved(vec!["a 3.1.2", "a 9.0.5"], vec!["a 3.1.5"]),
        ["a 3.1.2 -> a 3.1.5", "a 9.0.5 -> -"]
    );
    // Below 1.0 the minor version is the release line.
    assert_eq!(
        moved(vec!["a 0.4.4", "a 0.5.1"], vec!["a 0.4.5"]),
        ["a 0.4.4 -> a 0.4.5", "a 0.5.1 -> -"]
    );
    // A single copy that moves to another line is still a change: a
    // genuine downgrade stays visible, and holds the target back.
    assert_eq!(
        moved(vec!["a 0.5.1"], vec!["a 0.4.5"]),
        ["held a 0.5.1 -> a 0.4.5"]
    );
    // Epochs and a leading v belong to the line.
    assert_eq!(
        moved(vec!["a 1!2.0", "a v3.0"], vec!["a 1!2.1"]),
        ["a 1!2.0 -> a 1!2.1", "a v3.0 -> -"]
    );
}

#[test]
fn copies_of_one_version_pair_once() {
    // npm lists each installed copy: four copies of 1.0.2 moved to 1.0.4;
    // none became 0.1.8.
    assert_eq!(
        moved(
            vec!["a 1.0.2", "a 1.0.2", "a 1.0.2", "a 0.1.6"],
            vec!["a 1.0.4", "a 0.1.8", "a 0.1.8"]
        ),
        ["a 0.1.6 -> a 0.1.8", "a 1.0.2 -> a 1.0.4"]
    );
}

/// A resolution that downgrades a package is no update (ADR 0008): the
/// target proposes no change and names each downgrade.
#[test]
fn a_downgrade_holds_the_whole_target_back() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("multi.json"), "{}").unwrap();
    fs::write(root.path().join("multi.lock"), "old\n").unwrap();
    let engine = Engine::new(vec![Box::new(Multi {
        before: vec!["a 2.0", "b 1.0"],
        after: vec!["a 1.5", "b 1.1"],
    })]);
    let proposal = engine
        .prepare(
            root.path(),
            &["multi:multi.json".into()],
            UpdateOptions::default(),
        )
        .unwrap();
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    assert!(proposal.changes.is_empty(), "{:?}", proposal.changes);
    assert!(proposal.dependencies.is_empty());
    let held: Vec<_> = proposal
        .unresolved
        .iter()
        .map(|u| {
            (
                u.target.as_str(),
                u.package.as_str(),
                u.reference.as_str(),
                u.reason.as_str(),
            )
        })
        .collect();
    assert_eq!(
        held,
        [(
            "multi:multi.json",
            "a",
            "a 2.0 -> 1.5",
            "no upgrade fix for now: resolving would downgrade it"
        )]
    );
    assert!(
        proposal.validation.iter().any(|v| v.contains("held back")),
        "{:?}",
        proposal.validation
    );
    // A held-back target is never quietly up to date: check exits 6.
    assert_eq!(proposal.held_back, ["multi:multi.json"]);
    assert_eq!(proposal.exit_code(true), 6);
    assert_eq!(proposal.exit_code(false), 0);
}

/// Allowing downgrades is the user's explicit choice: the proposal goes
/// ahead and still names each downgrade.
#[test]
fn allowed_downgrades_are_proposed_and_named() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("multi.json"), "{}").unwrap();
    fs::write(root.path().join("multi.lock"), "old\n").unwrap();
    let engine = Engine::new(vec![Box::new(Multi {
        before: vec!["a 2.0", "b 1.0"],
        after: vec!["a 1.5", "b 1.1"],
    })]);
    let options = UpdateOptions {
        allow_downgrades: true,
        ..Default::default()
    };
    let proposal = engine
        .prepare(root.path(), &["multi:multi.json".into()], options)
        .unwrap();
    assert_eq!(proposal.changes.len(), 1, "{:?}", proposal.validation);
    assert!(proposal.unresolved.is_empty());
    assert!(
        proposal
            .validation
            .iter()
            .any(|v| v.contains("downgrades allowed: a 2.0 -> 1.5")),
        "{:?}",
        proposal.validation
    );
}

/// Prepares `multi.json` with fixed inventories and `options`.
fn prepare_multi(
    before: Vec<&'static str>,
    after: Vec<&'static str>,
    options: UpdateOptions,
) -> depsmith_core::Proposal {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("multi.json"), "{}").unwrap();
    fs::write(root.path().join("multi.lock"), "old\n").unwrap();
    Engine::new(vec![Box::new(Multi { before, after })])
        .prepare(root.path(), &["multi:multi.json".into()], options)
        .unwrap()
}

/// A version the ecosystem's scheme cannot read (a Git commit, an unknown
/// version) is not ordered, so it never holds a target back.
#[test]
fn unreadable_versions_are_not_downgrades() {
    let proposal = prepare_multi(vec!["a 2.0"], vec!["a unknown"], UpdateOptions::default());
    assert!(proposal.unresolved.is_empty(), "{:?}", proposal.unresolved);
    assert_eq!(proposal.changes.len(), 1);
}

/// A held-back target keeps its current lock, so a scan and its policy see
/// that lock: a vulnerable baseline still fails `--fail-on`.
#[cfg(unix)]
#[test]
fn a_held_back_target_is_still_scanned_and_gated() {
    let tools = tempfile::tempdir().unwrap();
    let grype = support::grype(
        tools.path(),
        "GHSA-test-0001",
        "github:language:python",
        "pkg:pypi/b@1.0",
        "1.1",
    );
    let options = UpdateOptions {
        scan: true,
        fail_on: Some("high".into()),
        tools: BTreeMap::from([("grype".into(), grype)]),
        ..Default::default()
    };
    let proposal = prepare_multi(vec!["a 2.0", "b 1.0"], vec!["a 1.5", "b 1.1"], options);
    assert!(proposal.changes.is_empty());
    assert_eq!(proposal.scans.len(), 1, "{:?}", proposal.failures);
    assert!(
        proposal.failures[0]
            .message
            .contains("vulnerability threshold exceeded"),
        "{:?}",
        proposal.failures
    );
}

//! A lock can hold several versions of one package (Python forks, npm
//! nesting, cargo majors): only versions that moved are paired, newest with
//! newest.
use depsmith_core::{
    adapter::{Adapter, AdapterSpec, Candidate, Capabilities, ManagedFiles},
    Engine, Package, Result, Target, UpdateOptions,
};
use std::{fs, path::Path};

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
                artifact: format!("https://example.org/{name}-{version}.tar.gz"),
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

/// The proposal's dependency changes as `before -> after` lines.
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
    // Environment a downgrades numpy; environment b upgrades it.
    assert_eq!(
        moved(
            vec!["numpy 2.3.5 a", "numpy 2.2.6 b"],
            vec!["numpy 2.2.0 a", "numpy 2.4.0 b"]
        ),
        ["numpy 2.2.6 -> numpy 2.4.0", "numpy 2.3.5 -> numpy 2.2.0"]
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

//! Suggestions and acceptance come from the engine for any adapter that
//! reports its declarations, rewrites them and names its registries: the
//! adapter below has no suggestion or acceptance logic of its own.
use depsmith_core::{
    adapter::{Adapter, AdapterSpec, Candidate, Capabilities, Support},
    conformance,
    constraints::{AvailabilityConfig, Declaration, Edit, FixtureRelease, RegistryConfig},
    Engine, Proposal, Result, Target, UpdateOptions,
};
use std::{collections::BTreeMap, fs, path::Path};

/// `reqs.json` holds `{"requires": {"name": "requirement", ...}}` with PyPI
/// requirements; its registry is an inline fixture.
struct Reqs;

fn read(root: &Path, target: &Target) -> Result<BTreeMap<String, String>> {
    let text = fs::read_to_string(root.join(&target.manifest))?;
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    Ok(serde_json::from_value(value["requires"].clone()).unwrap())
}

impl Adapter for Reqs {
    fn spec(&self) -> AdapterSpec {
        AdapterSpec {
            ecosystems: vec!["pypi".into()],
            capabilities: Capabilities {
                package_selection: Support::Supported,
                suggestion_acceptance: Support::Supported,
                ..Capabilities::undeclared()
            },
            ..AdapterSpec::new("reqs", &["reqs.json"])
        }
    }
    fn detects(&self, _: &Path, content: &str) -> bool {
        content.contains("\"requires\"")
    }
    fn select(
        &self,
        root: &Path,
        target: &Target,
        requested: &[String],
    ) -> Result<BTreeMap<String, String>> {
        let declared = read(root, target)?;
        Ok(requested
            .iter()
            .filter(|r| declared.contains_key(*r))
            .map(|r| (r.clone(), r.clone()))
            .collect())
    }
    fn declarations(&self, root: &Path, target: &Target) -> Result<Vec<Declaration>> {
        Ok(read(root, target)?
            .into_iter()
            .map(|(package, requirement)| Declaration {
                ecosystem: "pypi".into(),
                location: format!("requires.{package}"),
                package,
                requirement,
                file: target.manifest.clone(),
            })
            .collect())
    }
    fn rewrite(&self, stage: &Path, target: &Target, edits: &[Edit]) -> Result<()> {
        let mut declared = read(stage, target)?;
        for edit in edits {
            declared.insert(edit.declaration.package.clone(), edit.requirement.clone());
        }
        let text = serde_json::to_string(&serde_json::json!({ "requires": declared })).unwrap();
        fs::write(stage.join(&target.manifest), text)?;
        Ok(())
    }
    fn availability(&self, _: &Path, _: &Target) -> Result<AvailabilityConfig> {
        let release = |version: &str| FixtureRelease {
            version: version.into(),
            url: format!("https://example.invalid/six-{version}.whl"),
            sha256: format!("sha-{version}"),
        };
        Ok(AvailabilityConfig {
            registries: [(
                "pypi".to_string(),
                RegistryConfig::Fixture {
                    releases: [(
                        "six".to_string(),
                        vec![release("1.15.0"), release("1.16.0"), release("1.17.0")],
                    )]
                    .into(),
                    failing: vec!["flaky".into()],
                },
            )]
            .into(),
            exclude_newer: None,
        })
    }
    fn prepare(&self, _: &Path, target: &Target, _: &UpdateOptions) -> Result<Candidate> {
        Ok(Candidate {
            files: vec![target.manifest.clone()],
            ..Default::default()
        })
    }
}

fn prepare(requires: &str, accept: &[&str]) -> Proposal {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("reqs.json"),
        format!("{{\"requires\": {requires}}}"),
    )
    .unwrap();
    let options = UpdateOptions {
        accept: accept.iter().map(|a| a.to_string()).collect(),
        ..Default::default()
    };
    Engine::new(vec![Box::new(Reqs)])
        .prepare(root.path(), &["reqs:reqs.json".into()], options)
        .unwrap()
}

#[test]
fn capped_constraints_become_evidenced_suggestions() {
    let proposal = prepare(r#"{"six": "==1.15.0", "idna": ">=2", "flaky": "<1"}"#, &[]);
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    let by_name: BTreeMap<_, _> = proposal
        .suggestions
        .iter()
        .map(|s| (s.package.as_str(), s))
        .collect();
    // Lower bounds never suggest; a failed lookup is kept as not established.
    assert_eq!(
        by_name.keys().copied().collect::<Vec<_>>(),
        ["flaky", "six"]
    );
    let six = by_name["six"];
    assert_eq!(six.requirement, "==1.15.0");
    assert!(
        six.reason.contains("excludes a newer release"),
        "{}",
        six.reason
    );
    assert_eq!(
        six.evidence,
        ["fixture: 1.17.0 is excluded (newest allowed 1.15.0): https://example.invalid/six-1.17.0.whl sha256:sha-1.17.0"]
    );
    assert!(by_name["flaky"].reason.contains("has not been established"));
}

#[test]
fn accepting_restyles_to_the_newest_excluded_release() {
    let proposal = prepare(r#"{"six": "==1.15.0"}"#, &["six"]);
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    assert!(
        proposal.changes[0].after.contains("\"six\":\"==1.17.0\""),
        "{}",
        proposal.changes[0].after
    );
    assert!(proposal.validation.iter().any(|v| v
        == "reqs:reqs.json: accepted six ==1.15.0 -> ==1.17.0 (fixture: 1.17.0 is excluded (newest allowed 1.15.0): https://example.invalid/six-1.17.0.whl sha256:sha-1.17.0)"),
        "{:?}", proposal.validation);
    assert!(
        proposal.suggestions.is_empty(),
        "{:?}",
        proposal.suggestions
    );
}

#[test]
fn explicit_and_ambiguous_acceptances() {
    let proposal = prepare(r#"{"six": "==1.15.0"}"#, &["six===1.16.0"]);
    assert!(proposal.changes[0].after.contains("\"six\":\"==1.16.0\""));
    assert!(proposal
        .validation
        .iter()
        .any(|v| v.ends_with("accepted six ==1.15.0 -> ==1.16.0 (explicit replacement)")));
    for (requires, accept, code, needle) in [
        (r#"{"six": ">=1,<2"}"#, "six", 2, "--accept six=REQUIREMENT"),
        (r#"{"six": "==1.15.0"}"#, "six=1.16", 2, "PEP 440"),
        (r#"{"six": "==1.17.0"}"#, "six", 2, "nothing to accept"),
        (
            r#"{"flaky": "==0.9"}"#,
            "flaky",
            3,
            "could not be established",
        ),
    ] {
        let proposal = prepare(requires, &[accept]);
        assert_eq!(
            proposal.failures.len(),
            1,
            "{accept}: {:?}",
            proposal.failures
        );
        let failure = &proposal.failures[0];
        assert_eq!(failure.code, code, "{accept}: {}", failure.message);
        assert!(
            failure.message.contains(needle),
            "{accept}: {}",
            failure.message
        );
    }
}

#[test]
fn declarations_round_trip_through_rewrite_in_the_conformance_suite() {
    conformance::check(
        &|| Box::new(Reqs) as Box<dyn Adapter>,
        &conformance::Fixture {
            // Compact, as this adapter writes it: rewriting must not change
            // the layout of an unchanged file.
            files: vec![(
                "reqs.json".into(),
                r#"{"requires":{"six":"==1.15.0"}}"#.into(),
            )],
            target: "reqs:reqs.json".into(),
            declared: vec![("six".into(), "six".into())],
        },
    )
    .unwrap();
}

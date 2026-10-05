//! Commit pins for GitHub Actions: tag references get a "pin to commit"
//! suggestion citing the release commit, and `--accept OWNER/REPO` rewrites
//! them to `@<sha> # <release>`, which later updates keep.
use depsmith_core::actions::{Actions, Release, ReleaseSource};
use depsmith_core::{Engine, Proposal, UpdateOptions};

const TARGET: &str = "github-actions:.github/workflows/ci.yml";

fn sha(c: char) -> String {
    c.to_string().repeat(40)
}

/// `v4` is a major alias on the same commit as `v4.2.1`; v5 is published
/// only when `major` is set.
struct Fixed {
    major: bool,
}
impl ReleaseSource for Fixed {
    fn releases(&self, _: &str, _: u64) -> depsmith_core::Result<Vec<Release>> {
        let mut releases = vec![("v4", 'a'), ("v4.2.1", 'a'), ("v4.1.0", 'c')];
        if self.major {
            releases.push(("v5.0.0", 'b'));
        }
        Ok(releases
            .into_iter()
            .map(|(tag, c)| Release {
                tag: tag.into(),
                sha: sha(c),
            })
            .collect())
    }
}

fn prepare(workflow: &str, accept: &[&str]) -> Proposal {
    prepare_with(Fixed { major: true }, workflow, accept)
}

/// Without a newer major, a bare `--accept` pins.
fn prepare_current(workflow: &str, accept: &[&str]) -> Proposal {
    prepare_with(Fixed { major: false }, workflow, accept)
}

fn prepare_with(source: Fixed, workflow: &str, accept: &[&str]) -> Proposal {
    let root = tempfile::tempdir().unwrap();
    let workflows = root.path().join(".github/workflows");
    std::fs::create_dir_all(&workflows).unwrap();
    std::fs::write(workflows.join("ci.yml"), workflow).unwrap();
    let engine = Engine::new(vec![Box::new(Actions {
        source: Box::new(source),
    })]);
    let options = UpdateOptions {
        accept: accept.iter().map(|a| a.to_string()).collect(),
        ..Default::default()
    };
    engine
        .prepare(root.path(), &[TARGET.into()], options)
        .unwrap()
}

fn workflow(steps: &[&str]) -> String {
    let mut text = "on: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n".to_owned();
    for step in steps {
        text.push_str(&format!("      - uses: {step}\n"));
    }
    text
}

#[test]
fn tag_references_get_a_commit_pin_suggestion() {
    let proposal = prepare_current(
        &workflow(&[
            "actions/checkout@v4",
            "actions/checkout@v4.1.0 # keep",
            &format!("actions/checkout@{} # v4.2.1", sha('a')),
        ]),
        &[],
    );
    let pins: Vec<_> = proposal
        .suggestions
        .iter()
        .filter(|s| s.reason.contains("--accept actions/checkout"))
        .collect();
    // v4.1.0 is updated to v4.2.1 first; suggestions describe the candidate.
    let requirements: Vec<_> = pins.iter().map(|s| s.requirement.as_str()).collect();
    assert_eq!(requirements, ["v4", "v4.2.1"], "{:?}", proposal.suggestions);
    for pin in &pins {
        assert_eq!(pin.package, "actions/checkout");
        assert!(
            pin.evidence[0].contains(&sha('a')) && pin.evidence[0].contains("v4.2.1"),
            "{:?}",
            pin.evidence
        );
    }
}

#[test]
fn accepting_pins_every_tag_reference_to_its_release_commit() {
    let proposal = prepare_current(
        &workflow(&[
            "actions/checkout@v4",
            "actions/checkout@v4.1.0 # keep",
            "org/other@v4",
        ]),
        &["actions/checkout"],
    );
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    let after = &proposal.changes[0].after;
    // v4.1.0 is pinned to its own commit, then updated within v4 keeping the pin style.
    assert_eq!(
        after
            .matches(&format!("actions/checkout@{} # v4.2.1\n", sha('a')))
            .count(),
        2,
        "{after}"
    );
    assert!(after.contains("org/other@v4\n"), "{after}");
    let notes: Vec<_> = proposal
        .validation
        .iter()
        .filter(|v| v.contains("accepted"))
        .collect();
    assert_eq!(notes.len(), 2, "{notes:?}");
    assert!(notes[0].ends_with(&format!(
        "accepted actions/checkout v4 -> {} (commit pin of v4.2.1)",
        sha('a')
    )));
    assert!(
        !proposal
            .suggestions
            .iter()
            .any(|s| s.reason.contains("--accept actions/checkout")),
        "{:?}",
        proposal.suggestions
    );
}

#[test]
fn explicit_and_impossible_acceptances() {
    let proposal = prepare(
        &workflow(&["actions/checkout@v4"]),
        &["actions/checkout=v5.0.0"],
    );
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    assert!(proposal.changes[0]
        .after
        .contains("actions/checkout@v5.0.0\n"));
    let pinned = format!("actions/checkout@{} # v4.2.1", sha('a'));
    let proposal = prepare_current(&workflow(&[&pinned]), &["actions/checkout"]);
    assert!(
        proposal.failures[0]
            .message
            .contains("already a commit pin"),
        "{:?}",
        proposal.failures
    );
    for (step, accept, code, needle) in [
        (
            "actions/checkout@main",
            "actions/checkout",
            3,
            "no release commit",
        ),
        (
            "actions/checkout@v4",
            "actions/checkout=v 5",
            2,
            "not a tag or commit",
        ),
    ] {
        let proposal = prepare(&workflow(&[step]), &[accept]);
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

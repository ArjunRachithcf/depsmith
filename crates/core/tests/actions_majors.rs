//! Newer major releases of GitHub Actions: the suggestion names the major,
//! and `--accept OWNER/REPO` moves to it in the reference's own style, while
//! `--accept OWNER/REPO=TAG` must name a published release.
use depsmith_core::actions::{Actions, Release, ReleaseSource};
use depsmith_core::{Engine, Proposal, UpdateOptions};

const TARGET: &str = "github-actions:.github/workflows/ci.yml";

fn sha(c: char) -> String {
    c.to_string().repeat(40)
}

/// v4 and v5 are both published, each with major and minor aliases.
struct Published;
impl ReleaseSource for Published {
    fn releases(&self, _: &str, _: u64) -> depsmith_core::Result<Vec<Release>> {
        Ok([
            ("v4", 'a'),
            ("v4.2", 'a'),
            ("v4.2.1", 'a'),
            ("v5", 'b'),
            ("v5.1", 'b'),
            ("v5.1.0", 'b'),
            ("v5.0.0", 'c'),
        ]
        .map(|(tag, c)| Release {
            tag: tag.into(),
            sha: sha(c),
        })
        .into())
    }
}

fn prepare(workflow: &str, accept: &[&str]) -> Proposal {
    let root = tempfile::tempdir().unwrap();
    let workflows = root.path().join(".github/workflows");
    std::fs::create_dir_all(&workflows).unwrap();
    std::fs::write(workflows.join("ci.yml"), workflow).unwrap();
    let engine = Engine::new(vec![Box::new(Actions {
        source: Box::new(Published),
    })]);
    let options = UpdateOptions {
        accept: accept.iter().map(|a| a.to_string()).collect(),
        ..Default::default()
    };
    engine
        .prepare(root.path(), &[TARGET.into()], options)
        .unwrap()
}

fn workflow(step: &str) -> String {
    format!(
        "on: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: {step}\n"
    )
}

fn accepted(step: &str, accept: &str) -> String {
    let proposal = prepare(&workflow(step), &[accept]);
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    proposal.changes[0].after.clone()
}

#[test]
fn accepting_moves_a_tag_to_the_newest_major_in_its_own_style() {
    for (from, to) in [
        ("actions/checkout@v4", "actions/checkout@v5\n"),
        ("actions/checkout@v4.2", "actions/checkout@v5.1\n"),
        ("actions/checkout@v4.2.1", "actions/checkout@v5.1.0\n"),
    ] {
        let after = accepted(from, "actions/checkout");
        assert!(after.contains(to), "{from}: {after}");
    }
}

#[test]
fn accepting_keeps_a_commit_pin_a_commit_pin_of_the_newest_major() {
    let after = accepted(
        &format!("actions/checkout@{} # v4.2.1", sha('a')),
        "actions/checkout",
    );
    assert!(
        after.contains(&format!("actions/checkout@{} # v5.1.0\n", sha('b'))),
        "{after}"
    );
}

#[test]
fn an_explicit_tag_must_be_a_release_and_keeps_a_commit_pin_a_pin() {
    let after = accepted("actions/checkout@v4", "actions/checkout=v5");
    assert!(after.contains("actions/checkout@v5\n"), "{after}");
    // The pin moves to v5.0.0's commit, then updates within v5 as pins do.
    let proposal = prepare(
        &workflow(&format!("actions/checkout@{} # v4.2.1", sha('a'))),
        &["actions/checkout=v5.0.0"],
    );
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    assert!(
        proposal.validation.iter().any(|v| v.ends_with(&format!(
            "accepted actions/checkout {} -> {} (commit pin of v5.0.0)",
            sha('a'),
            sha('c')
        ))),
        "{:?}",
        proposal.validation
    );
    assert!(
        proposal.changes[0]
            .after
            .contains(&format!("actions/checkout@{} # v5.1.0\n", sha('b'))),
        "{}",
        proposal.changes[0].after
    );
    let proposal = prepare(&workflow("actions/checkout@v4"), &["actions/checkout=v9"]);
    let failure = &proposal.failures[0];
    assert_eq!(failure.code, 2, "{}", failure.message);
    assert!(
        failure.message.contains("v9 is not a published release"),
        "{}",
        failure.message
    );
}

#[test]
fn the_suggestion_names_the_newer_major_and_how_to_accept_it() {
    let proposal = prepare(&workflow("actions/checkout@v4"), &[]);
    let major: Vec<_> = proposal
        .suggestions
        .iter()
        .filter(|s| s.reason.contains("newer major"))
        .collect();
    assert_eq!(major.len(), 1, "{:?}", proposal.suggestions);
    assert_eq!(major[0].requirement, "v5.1.0");
    assert!(
        major[0]
            .reason
            .contains("move to it with --accept actions/checkout"),
        "{}",
        major[0].reason
    );
    assert!(
        major[0].evidence[0].ends_with("/releases/tag/v5.1.0"),
        "{:?}",
        major[0].evidence
    );
    // Pinning then needs the major move first, so the pin hint says so.
    let pin = proposal
        .suggestions
        .iter()
        .find(|s| s.reason.contains("Pin this action"))
        .unwrap();
    assert!(
        pin.reason.contains("after moving to v5.1.0"),
        "{}",
        pin.reason
    );
}

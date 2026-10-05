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
            .contains("--accept actions/checkout moves @v4 to @v5"),
        "{}",
        major[0].reason
    );
    assert!(
        major[0].evidence[0].ends_with("/releases/tag/v5.1.0"),
        "{:?}",
        major[0].evidence
    );
    // Pinning waits for the move.
    assert!(
        !proposal
            .suggestions
            .iter()
            .any(|s| s.reason.contains("Pin this action")),
        "{:?}",
        proposal.suggestions
    );
}

fn workflow_of(steps: &[&str]) -> String {
    let mut text = "on: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n".to_owned();
    for step in steps {
        text.push_str(&format!("      - uses: {step}\n"));
    }
    text
}

#[test]
fn one_accept_only_moves_majors_and_leaves_current_references_alone() {
    let current_pin = format!("actions/checkout@{} # v5.1.0", sha('b'));
    let proposal = prepare(
        &workflow_of(&[
            "actions/checkout@v4",
            "actions/checkout@v5.1.0",
            &format!("actions/checkout@{} # v4.2.1", sha('a')),
            &current_pin,
        ]),
        &["actions/checkout"],
    );
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    let after = &proposal.changes[0].after;
    assert!(after.contains("actions/checkout@v5\n"), "{after}");
    // Already on the newest major: neither pinned nor an error.
    assert!(after.contains("actions/checkout@v5.1.0\n"), "{after}");
    assert_eq!(
        after.matches(&format!("{current_pin}\n")).count(),
        2,
        "{after}"
    );
}

#[test]
fn an_explicit_major_line_resolves_to_its_newest_release_in_the_declared_precision() {
    // There is no v5.0 alias, so v5.0 names a line, resolved to v5.0.0; the
    // update then moves the exact tag within v5, as for any reference.
    let proposal = prepare(
        &workflow("actions/checkout@v4.2.1"),
        &["actions/checkout=v5.0"],
    );
    assert!(
        proposal
            .validation
            .iter()
            .any(|v| v.ends_with("accepted actions/checkout v4.2.1 -> v5.0.0 (release v5.0.0)")),
        "{:?}",
        proposal.validation
    );
    // A published tag is written as given.
    let after = accepted("actions/checkout@v4.2.1", "actions/checkout=v5");
    assert!(after.contains("actions/checkout@v5\n"), "{after}");
    // A minor alias reference needs the alias tag itself.
    let proposal = prepare(
        &workflow("actions/checkout@v4.2"),
        &["actions/checkout=v5.0"],
    );
    assert!(
        proposal.failures[0].message.contains("has no tag v5.0"),
        "{:?}",
        proposal.failures
    );
}

#[test]
fn explicit_commits_are_written_as_given_and_unknown_versions_are_pinned_or_refused() {
    let after = accepted(
        "actions/checkout@v4",
        &format!("actions/checkout={}", sha('b')),
    );
    assert!(
        after.contains(&format!("actions/checkout@{}\n", sha('b'))),
        "{after}"
    );
    // A branch has no version to move from, so the bare form tries to pin it.
    let proposal = prepare(&workflow("actions/checkout@main"), &["actions/checkout"]);
    assert!(
        proposal.failures[0].message.contains("no release commit"),
        "{:?}",
        proposal.failures
    );
}

#[test]
fn upgrading_a_selected_repository_still_moves_it_to_the_newest_major() {
    let root = tempfile::tempdir().unwrap();
    let workflows = root.path().join(".github/workflows");
    std::fs::create_dir_all(&workflows).unwrap();
    std::fs::write(workflows.join("ci.yml"), workflow("actions/checkout@v4")).unwrap();
    let engine = Engine::new(vec![Box::new(Actions {
        source: Box::new(Published),
    })]);
    let options = UpdateOptions {
        packages: vec!["actions/checkout".into()],
        upgrade: true,
        ..Default::default()
    };
    let proposal = engine
        .prepare(root.path(), &[TARGET.into()], options)
        .unwrap();
    assert!(proposal.changes[0].after.contains("actions/checkout@v5\n"));
}

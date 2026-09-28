use depsmith_core::actions::{rewrite, rewrite_explained, Actions, Release, ReleaseSource};
use depsmith_core::{Engine, UpdateOptions};

fn releases() -> Vec<Release> {
    vec![
        Release {
            tag: "v4.2.1".into(),
            sha: "a".repeat(40),
        },
        Release {
            tag: "v5.0.0".into(),
            sha: "b".repeat(40),
        },
    ]
}
#[test]
fn updates_exact_tag_preserving_yaml_and_ignores_script_text() {
    let source = "name: test\njobs:\n  build:\n    steps:\n      - uses: actions/checkout@v4.1.0 # keep\n      - run: |\n          uses: actions/checkout@v4.1.0\n";
    let output = rewrite(source, "actions/checkout", &releases(), false).unwrap();
    assert!(output.contains("- uses: actions/checkout@v4.2.1 # keep"));
    assert!(output.contains("          uses: actions/checkout@v4.1.0"));
    assert!(!output.contains("@v5"));
}
#[test]
fn major_tag_remains_major_and_commit_pin_remains_sha() {
    let source = format!("jobs:\n  build:\n    steps:\n      - uses: actions/checkout@v4\n      - uses: actions/checkout@{} # v4.0.0\n", "c".repeat(40));
    let mut tags = releases();
    tags.push(Release {
        tag: "v4.0.0".into(),
        sha: "c".repeat(40),
    });
    let output = rewrite(&source, "actions/checkout", &tags, false).unwrap();
    assert!(output.contains("actions/checkout@v4\n"));
    assert!(output.contains(&format!("actions/checkout@{} # v4.2.1", "a".repeat(40))));
}
#[test]
fn reusable_workflow_updates_and_major_upgrade_is_explicit() {
    let source = "jobs:\n  reusable:\n    uses: org/repo/.github/workflows/build.yml@v4.0.0\n";
    let output = rewrite(source, "org/repo", &releases(), true).unwrap();
    assert!(output.contains("build.yml@v5.0.0"));
}
#[test]
fn never_invents_a_minor_tag_from_a_patch_release() {
    let source = "jobs:\n  build:\n    steps:\n      - uses: org/repo@v4.1\n";
    assert_eq!(
        rewrite(source, "org/repo", &releases(), false).unwrap(),
        source
    );
}

fn explained(reference: &str, major: bool) -> Vec<(String, String)> {
    let source = format!("jobs:\n  build:\n    steps:\n      - uses: org/repo@{reference}\n");
    let (output, unresolved) = rewrite_explained(&source, "org/repo", &releases(), major).unwrap();
    if !unresolved.is_empty() {
        assert_eq!(output, source, "unresolved references stay unchanged");
    }
    unresolved
}

#[test]
fn unresolvable_references_are_explained() {
    let cases = [
        ("main", "not a version tag or commit SHA"),
        (&*"d".repeat(40), "does not match any release"),
        ("v9.1.0", "no release at or above 9.1.0 in major 9"),
        ("v4.1", "tag v4.2 does not exist"),
    ];
    for (reference, reason) in cases {
        let unresolved = explained(reference, false);
        assert_eq!(unresolved.len(), 1, "{reference}: {unresolved:?}");
        assert_eq!(unresolved[0].0, format!("org/repo@{reference}"));
        assert!(
            unresolved[0].1.contains(reason),
            "{reference}: {}",
            unresolved[0].1
        );
    }
}

#[test]
fn current_and_updated_references_need_no_explanation() {
    assert!(explained("v4.2.1", false).is_empty());
    assert!(explained("v4.1.0", false).is_empty());
    assert!(explained("v5.0.0", true).is_empty());
}

struct Fixed;
impl ReleaseSource for Fixed {
    fn releases(&self, _: &str, _: u64) -> depsmith_core::Result<Vec<Release>> {
        Ok(releases())
    }
}

#[test]
fn proposal_reports_unresolved_references_per_target() {
    let root = tempfile::tempdir().unwrap();
    let workflows = root.path().join(".github/workflows");
    std::fs::create_dir_all(&workflows).unwrap();
    std::fs::write(
        workflows.join("ci.yml"),
        "on: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: org/repo@main\n",
    )
    .unwrap();
    let engine = Engine::new(vec![Box::new(Actions {
        source: Box::new(Fixed),
    })]);
    let proposal = engine
        .prepare(
            root.path(),
            &["github-actions:.github/workflows/ci.yml".into()],
            UpdateOptions::default(),
        )
        .unwrap();
    assert!(proposal.changes.is_empty());
    assert_eq!(proposal.unresolved.len(), 1, "{:?}", proposal.unresolved);
    let entry = &proposal.unresolved[0];
    assert_eq!(entry.target, "github-actions:.github/workflows/ci.yml");
    assert_eq!(entry.package, "org/repo");
    assert_eq!(entry.reference, "org/repo@main");
}

#[test]
fn version_comment_matching_old_tag_follows_the_update() {
    let source = "jobs:\n  build:\n    steps:\n      - uses: org/repo@v4.1.0 # v4.1.0\n      - uses: org/repo@v4.1.0 # pinned for reasons\n";
    let output = rewrite(source, "org/repo", &releases(), false).unwrap();
    assert!(output.contains("org/repo@v4.2.1 # v4.2.1\n"), "{output}");
    assert!(
        output.contains("org/repo@v4.2.1 # pinned for reasons\n"),
        "{output}"
    );
}

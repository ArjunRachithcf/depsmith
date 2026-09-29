//! Opt-in acceptance test against the live GitHub API. Uses GITHUB_TOKEN or
//! GH_TOKEN when set (anonymous requests are rate-limited).
use depsmith_core::{Engine, UpdateOptions};

/// actions/setup-python v4.0.0.
const SETUP_PYTHON_V4_0_0: &str = "d09bd5e6005b175076f227b13d9730d56e9dcfcb";

/// The reference `uses: REPO@REF` resolves to in `text`.
fn reference<'a>(text: &'a str, repository: &str) -> &'a str {
    let start = text.find(&format!("{repository}@")).unwrap() + repository.len() + 1;
    text[start..].split_whitespace().next().unwrap()
}

/// `vMAJOR.MINOR.PATCH` as numbers.
fn version(tag: &str) -> (u64, u64, u64) {
    let mut parts = tag
        .trim_start_matches('v')
        .split('.')
        .map(|p| p.parse().unwrap());
    (
        parts.next().unwrap(),
        parts.next().unwrap(),
        parts.next().unwrap(),
    )
}

#[test]
#[ignore = "queries the live GitHub API"]
fn live_actions_stay_in_their_major_line_and_offer_major_upgrades() {
    let root = tempfile::tempdir().unwrap();
    let workflows = root.path().join(".github/workflows");
    std::fs::create_dir_all(&workflows).unwrap();
    let generator =
        "slsa-framework/slsa-github-generator/.github/workflows/generator_generic_slsa3.yml";
    std::fs::write(
        workflows.join("ci.yml"),
        format!(
            "on: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      \
             - uses: actions/checkout@v3.5.0\n      \
             - uses: actions/setup-python@{SETUP_PYTHON_V4_0_0} # v4.0.0\n  \
             provenance:\n    uses: {generator}@v1.9.0\n"
        ),
    )
    .unwrap();
    let proposal = Engine::default()
        .prepare(
            root.path(),
            &["github-actions:.github/workflows/ci.yml".into()],
            UpdateOptions::default(),
        )
        .unwrap();
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    assert!(proposal.unresolved.is_empty(), "{:?}", proposal.unresolved);
    let after = &proposal.changes[0].after;

    // Exact tag: newest release in the same major line.
    let checkout = version(reference(after, "actions/checkout"));
    assert!(checkout.0 == 3 && checkout > (3, 5, 0), "{checkout:?}");

    // Commit pin: stays a full SHA; the version comment follows it.
    let pin = reference(after, "actions/setup-python");
    assert!(pin.len() == 40 && pin != SETUP_PYTHON_V4_0_0, "{pin}");
    let comment = after
        .lines()
        .find(|l| l.contains(pin))
        .and_then(|l| l.split("# ").nth(1))
        .unwrap();
    let pinned = version(comment.trim());
    assert!(pinned.0 == 4 && pinned > (4, 0, 0), "{comment}");

    // Reusable workflow: same major line.
    let reusable = version(reference(after, generator));
    assert!(reusable.0 == 1 && reusable > (1, 9, 0), "{reusable:?}");

    // Newer major lines are separate, explicit suggestions.
    for repository in [
        "actions/checkout",
        "actions/setup-python",
        "slsa-framework/slsa-github-generator",
    ] {
        assert!(
            proposal.suggestions.iter().any(|s| s.package == repository),
            "no major-upgrade suggestion for {repository}: {:?}",
            proposal.suggestions
        );
    }
}

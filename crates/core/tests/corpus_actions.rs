//! The corpus harness's GitHub Actions driver (`scripts/corpus/run.py`) for
//! hosts that cannot reach the GitHub REST API: the real adapter prepares,
//! applies and re-checks a workflow in a real repository, with releases read
//! from `git ls-remote --tags` (a plain `vX.Y.Z` tag counts as stable).
//! Ignored by default, and does nothing without `CORPUS_ROOT`; the harness
//! sets it and `CORPUS_TARGET`.
use depsmith_core::{
    actions::{Actions, Release, ReleaseSource},
    apply, Engine, Result, UpdateOptions,
};
use std::{collections::BTreeMap, path::PathBuf, process::Command};

struct GitTags;
impl ReleaseSource for GitTags {
    fn releases(&self, repository: &str, _timeout: u64) -> Result<Vec<Release>> {
        let output = Command::new("git")
            .args(["ls-remote", "--tags"])
            .arg(format!("https://github.com/{repository}"))
            .output()?;
        if !output.status.success() {
            return Err(depsmith_core::Error::Operation(format!(
                "git ls-remote {repository} failed"
            )));
        }
        // An annotated tag's `^{}` line names the commit it points to.
        let mut tags = BTreeMap::new();
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let Some((sha, name)) = line.split_once('\t') else {
                continue;
            };
            let name = name.trim_start_matches("refs/tags/");
            match name.strip_suffix("^{}") {
                Some(tag) => {
                    tags.insert(tag.to_owned(), sha.to_owned());
                }
                None => {
                    tags.entry(name.to_owned())
                        .or_insert_with(|| sha.to_owned());
                }
            }
        }
        let stable = |tag: &str| {
            let digits = tag.strip_prefix('v').unwrap_or(tag);
            !digits.is_empty()
                && digits.split('.').count() <= 3
                && digits
                    .split('.')
                    .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        };
        Ok(tags
            .into_iter()
            .filter(|(tag, _)| stable(tag))
            .map(|(tag, sha)| Release { tag, sha })
            .collect())
    }
}

#[test]
#[ignore = "driven by the corpus harness"]
fn update_a_workflow_and_check_it_again() {
    // Native integration runs every ignored test; outside the harness
    // there is no repository to drive.
    let Some(root) = std::env::var_os("CORPUS_ROOT").map(PathBuf::from) else {
        eprintln!("CORPUS_ROOT is not set; nothing to drive");
        return;
    };
    let target = std::env::var("CORPUS_TARGET").expect("CORPUS_TARGET");
    let engine = Engine::new(vec![Box::new(Actions {
        source: Box::new(GitTags),
    })]);
    let selected = [target];
    let proposal = engine
        .prepare(&root, &selected, UpdateOptions::default())
        .expect("prepare");
    assert!(
        proposal.failures.is_empty(),
        "CORPUS_FAILURE {}",
        proposal.failures[0].message
    );
    let applied = apply(&proposal, false).expect("apply");
    let again = engine
        .prepare(&root, &selected, UpdateOptions::default())
        .expect("prepare again");
    assert!(
        again.failures.is_empty(),
        "CORPUS_FAILURE after apply: {}",
        again.failures[0].message
    );
    assert!(
        again.changes.is_empty(),
        "CORPUS_FAILURE updates still pending after apply:\n{}",
        again.changes[0].diff
    );
    println!(
        "CORPUS_ACTIONS changes={} dependencies={} suggestions={} unresolved={} written={}",
        proposal.changes.len(),
        proposal.dependencies.len(),
        proposal.suggestions.len(),
        proposal.unresolved.len(),
        applied.applied.len()
    );
}

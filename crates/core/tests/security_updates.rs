//! Security updates (ADR 0004) through the engine, with stand-ins for uv
//! and Grype: only packages with actionable findings move, each toward its
//! fix version, and the candidate scan decides what was fixed.
#![cfg(unix)]
use depsmith_core::{
    constraints::{FixtureRelease, RegistryConfig},
    uv::Uv,
    Engine, Error, FixOutcome, Proposal, SecurityFix, SecurityTarget, UpdateOptions,
};
use serde_json::json;
use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt};

const PYPROJECT: &str = "[project]\nname = \"p\"\nversion = \"0.1.0\"\ndependencies = [\n    \"six==1.15.0\",\n    \"idna\",\n]\n";

/// A uv.lock with the given versions; `urllib3` is only a dependency of idna
/// here, so it is transitive.
fn lock(six: &str, idna: &str, urllib3: &str) -> String {
    let package = |name: &str, version: &str, dependencies: &str| {
        format!(
            "[[package]]\nname = \"{name}\"\nversion = \"{version}\"\nsource = {{ registry = \"https://pypi.org/simple\" }}\n{dependencies}sdist = {{ url = \"https://files.pythonhosted.org/{name}-{version}.tar.gz\" }}\n\n"
        )
    };
    format!(
        "version = 1\nrevision = 3\n\n[[package]]\nname = \"p\"\nversion = \"0.1.0\"\nsource = {{ virtual = \".\" }}\ndependencies = [{{ name = \"idna\" }}, {{ name = \"six\" }}]\n\n{}{}{}",
        package("six", six, ""),
        package("idna", idna, "dependencies = [{ name = \"urllib3\" }]\n"),
        package("urllib3", urllib3, "")
    )
}

/// One Grype match for `name@version`, fixed in `fixes`.
fn advisory(
    id: &str,
    name: &str,
    version: &str,
    severity: &str,
    fixes: &[&str],
) -> (String, String) {
    let state = if fixes.is_empty() {
        "not-fixed"
    } else {
        "fixed"
    };
    let purl = format!("pkg:pypi/{name}@{version}");
    let row = json!({"artifact": {"purl": purl}, "matchDetails": [],
        "vulnerability": {"id": id, "namespace": "github:language:python", "severity": severity,
            "fix": {"versions": fixes, "state": state}}});
    (purl, row.to_string())
}

struct Run {
    result: depsmith_core::Result<Proposal>,
    /// The arguments of every uv `lock` invocation, one line each.
    locks: Vec<String>,
}

/// Prepare a security update of a uv target whose lock is `baseline`, with
/// a uv that writes `next` and a Grype that reports `advisories` for any
/// scanned package they match.
fn prepare(
    baseline: &str,
    next: &str,
    advisories: &[(String, String)],
    options: UpdateOptions,
) -> Run {
    prepare_with_releases(PYPROJECT, baseline, next, advisories, options, &[], &[])
}

/// [`prepare`] of the project `pyproject`, with an index that publishes
/// `releases` of each package and fails for the `failing` ones.
fn prepare_with_releases(
    pyproject: &str,
    baseline: &str,
    next: &str,
    advisories: &[(String, String)],
    options: UpdateOptions,
    releases: &[(&str, &[&str])],
    failing: &[&str],
) -> Run {
    prepare_scripted(
        pyproject,
        baseline,
        next,
        advisories,
        options,
        releases,
        failing,
        &|_| String::new(),
    )
}

/// [`prepare_with_releases`] with a uv that finds no solution whenever its
/// arguments contain `refuse`.
#[allow(clippy::too_many_arguments)]
fn prepare_refusing(
    pyproject: &str,
    baseline: &str,
    next: &str,
    advisories: &[(String, String)],
    options: UpdateOptions,
    releases: &[(&str, &[&str])],
    failing: &[&str],
    refuse: &str,
) -> Run {
    let refusal = format!("case \"$*\" in *'{refuse}'*) printf \"      we can conclude that your project's requirements are\\n      unsatisfiable.\\n\" >&2; exit 1;; esac\n");
    prepare_scripted(
        pyproject,
        baseline,
        next,
        advisories,
        options,
        releases,
        failing,
        &|_| refusal.clone(),
    )
}

/// [`prepare_with_releases`] with `prefix(tools directory)` run first by the
/// uv stand-in, to answer some invocations differently.
#[allow(clippy::too_many_arguments)]
fn prepare_scripted(
    pyproject: &str,
    baseline: &str,
    next: &str,
    advisories: &[(String, String)],
    options: UpdateOptions,
    releases: &[(&str, &[&str])],
    failing: &[&str],
    prefix: &dyn Fn(&std::path::Path) -> String,
) -> Run {
    let root = tempfile::tempdir().unwrap();
    let tools = tempfile::tempdir().unwrap();
    fs::write(root.path().join("pyproject.toml"), pyproject).unwrap();
    fs::write(root.path().join("uv.lock"), baseline).unwrap();
    let dir = tools.path();
    let refusal = prefix(dir);
    fs::write(dir.join("next.lock"), next).unwrap();
    let rules: String = advisories
        .iter()
        .map(|(purl, row)| format!("{purl}\t{row}\n"))
        .collect();
    fs::write(dir.join("rules"), rules).unwrap();
    let script = |name: &str, body: String| {
        let path = dir.join(name);
        fs::write(&path, body).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    };
    let uv = script(
        "uv",
        format!(
            "#!/bin/sh\n{refusal}case \"$1\" in\n  --version) echo 'uv 0.12.15';;\n  lock) echo \"$*\" >> '{d}/locks'; case \"$*\" in *--locked*) cmp -s '{d}/next.lock' uv.lock || exit 1;; *) cp '{d}/next.lock' uv.lock;; esac;;\n  *) exit 2;;\nesac\n",
            d = dir.display()
        ),
    );
    let grype = script(
        "grype",
        format!(
            "#!/bin/sh\ncase \"$1\" in\n  db) [ \"$2\" = status ] && echo '{{\"built\":\"2026-10-01T00:00:00Z\"}}'; exit 0;;\n  sbom:*) f=\"${{1#sbom:}}\"; printf '{{\"matches\":['; sep=''\n    while IFS='\t' read -r purl row; do\n      if grep -q \"\\\"purl\\\":\\\"$purl\\\"\" \"$f\"; then printf '%s%s' \"$sep\" \"$row\"; sep=','; fi\n    done < '{d}/rules'\n    printf ']}}\\n'; exit 0;;\nesac\nexit 2\n",
            d = dir.display()
        ),
    );
    let options = UpdateOptions {
        tools: BTreeMap::from([("uv".into(), uv), ("grype".into(), grype)]),
        security_only: true,
        ..options
    };
    // An offline registry, for evidence and the releases patch aims at.
    let registry = RegistryConfig::Fixture {
        releases: releases
            .iter()
            .map(|(name, versions)| {
                let release = |version: &&str| FixtureRelease {
                    version: version.to_string(),
                    url: format!("https://files.pythonhosted.org/{name}-{version}.tar.gz"),
                    sha256: format!("sha256-{version}"),
                };
                (name.to_string(), versions.iter().map(release).collect())
            })
            .collect(),
        failing: failing.iter().map(|name| name.to_string()).collect(),
    };
    let uv = Uv {
        registry: Some(registry),
    };
    let result = Engine::new(vec![Box::new(uv)]).prepare(
        root.path(),
        &["uv:pyproject.toml".into()],
        options,
    );
    let locks = fs::read_to_string(dir.join("locks"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect();
    Run { result, locks }
}

fn fix<'a>(proposal: &'a Proposal, advisory: &str) -> &'a SecurityFix {
    proposal
        .security
        .iter()
        .find(|f| f.advisory == advisory)
        .unwrap_or_else(|| panic!("no {advisory} in {:?}", proposal.security))
}

#[test]
fn only_vulnerable_direct_packages_move_to_their_lowest_fix() {
    let run = prepare(
        &lock("1.15.0", "3.6", "2.0.0"),
        &lock("1.15.0", "3.7", "2.0.0"),
        &[advisory(
            "GHSA-idna",
            "idna",
            "3.6",
            "Medium",
            &["3.7", "3.8"],
        )],
        UpdateOptions::default(),
    );
    let proposal = run.result.unwrap();
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    // Only the vulnerable package is asked for, at its exact fix version.
    let first = &run.locks[0];
    assert!(first.contains("--upgrade-package idna==3.7"), "{first}");
    assert!(!first.contains("six"), "{first}");
    let idna = fix(&proposal, "GHSA-idna");
    assert_eq!(idna.outcome, FixOutcome::Fixed);
    assert_eq!(idna.fix_version.as_deref(), Some("3.7"));
    assert_eq!(idna.version, "3.6");
    assert_eq!(proposal.exit_code(true), 1);
    assert_eq!(proposal.scans[0].resolved.len(), 1);
}

#[test]
fn a_constraint_below_the_fix_is_a_blocked_fix_naming_its_declaration() {
    let run = prepare(
        &lock("1.15.0", "3.7", "2.0.0"),
        &lock("1.15.0", "3.7", "2.0.0"),
        &[advisory("GHSA-six", "six", "1.15.0", "High", &["1.16.0"])],
        UpdateOptions::default(),
    );
    let proposal = run.result.unwrap();
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    // The declaration excludes the fix, so the tool is not asked for it.
    assert!(run.locks.is_empty(), "{:?}", run.locks);
    let six = fix(&proposal, "GHSA-six");
    assert_eq!(six.outcome, FixOutcome::Blocked);
    let suggestion = proposal
        .suggestions
        .iter()
        .find(|s| s.package == "six" && s.reason.contains("GHSA-six"))
        .expect("a blocked-fix suggestion");
    assert_eq!(suggestion.requirement, "==1.15.0");
    assert!(
        suggestion.reason.contains("GHSA-six")
            && suggestion.reason.contains("1.16.0")
            && suggestion.reason.contains("--accept six="),
        "{}",
        suggestion.reason
    );
    assert!(suggestion.evidence.iter().any(|e| e.contains("GHSA-six")));
}

#[test]
fn unfixable_findings_are_reported_without_moving_anything() {
    let run = prepare(
        &lock("1.15.0", "3.7", "2.0.0"),
        &lock("1.15.0", "3.7", "2.0.0"),
        &[advisory("GHSA-open", "idna", "3.7", "High", &[])],
        UpdateOptions::default(),
    );
    let proposal = run.result.unwrap();
    assert!(run.locks.is_empty(), "nothing to move: {:?}", run.locks);
    assert!(proposal.changes.is_empty());
    assert_eq!(fix(&proposal, "GHSA-open").outcome, FixOutcome::Unfixable);
    // The baseline scan is still reported.
    assert_eq!(proposal.scans.len(), 1);
    assert_eq!(proposal.exit_code(true), 0);
}

#[test]
fn a_transitive_package_moves_to_its_fix_and_the_fix_names_its_path() {
    let run = prepare(
        &lock("1.15.0", "3.7", "2.0.0"),
        &lock("1.15.0", "3.7", "2.0.7"),
        &[advisory(
            "GHSA-transitive",
            "urllib3",
            "2.0.0",
            "High",
            &["2.0.7"],
        )],
        UpdateOptions::default(),
    );
    let proposal = run.result.unwrap();
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    let first = &run.locks[0];
    assert!(
        first.contains("--upgrade-package urllib3==2.0.7"),
        "{first}"
    );
    assert!(!first.contains("idna"), "{first}");
    let transitive = fix(&proposal, "GHSA-transitive");
    assert_eq!(transitive.outcome, FixOutcome::Fixed);
    assert_eq!(
        transitive.paths,
        [vec!["idna".to_owned(), "urllib3".to_owned()]]
    );
}

#[test]
fn a_transitive_package_left_below_its_fix_is_a_suggestion_naming_its_path() {
    let run = prepare(
        &lock("1.15.0", "3.7", "2.0.0"),
        &lock("1.15.0", "3.7", "2.0.0"),
        &[advisory(
            "GHSA-transitive",
            "urllib3",
            "2.0.0",
            "High",
            &["2.0.7"],
        )],
        UpdateOptions::default(),
    );
    let proposal = run.result.unwrap();
    assert!(run.locks[0].contains("--upgrade-package urllib3==2.0.7"));
    let transitive = fix(&proposal, "GHSA-transitive");
    assert_eq!(transitive.outcome, FixOutcome::Blocked);
    assert_eq!(
        transitive.paths,
        [vec!["idna".to_owned(), "urllib3".to_owned()]]
    );
    // uv.lock records no requirements, so only the introducer is known.
    let suggestion = proposal
        .suggestions
        .iter()
        .find(|s| s.reason.contains("GHSA-transitive"))
        .expect("a blocked-fix suggestion");
    assert_eq!(suggestion.package, "idna");
    assert!(
        suggestion.reason.contains("idna > urllib3") && suggestion.reason.contains("2.0.7"),
        "{}",
        suggestion.reason
    );
    assert!(
        !suggestion.reason.contains("--accept"),
        "{}",
        suggestion.reason
    );
    assert!(proposal
        .changes
        .iter()
        .all(|c| !c.path.ends_with("pyproject.toml")));
}

#[test]
fn a_fix_the_resolver_cannot_reach_is_blocked_and_the_others_still_move() {
    let run = prepare_refusing(
        PYPROJECT,
        &lock("1.15.0", "3.6", "2.0.0"),
        &lock("1.15.0", "3.7", "2.0.0"),
        &[
            advisory("GHSA-direct", "idna", "3.6", "High", &["3.7"]),
            advisory("GHSA-transitive", "urllib3", "2.0.0", "High", &["2.0.7"]),
        ],
        UpdateOptions::default(),
        &[],
        &[],
        "urllib3==2.0.7",
    );
    let proposal = run.result.unwrap();
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    assert_eq!(fix(&proposal, "GHSA-direct").outcome, FixOutcome::Fixed);
    assert_eq!(
        fix(&proposal, "GHSA-transitive").outcome,
        FixOutcome::Blocked
    );
    assert!(proposal
        .suggestions
        .iter()
        .any(|s| s.reason.contains("GHSA-transitive")));
}

#[test]
fn a_fix_that_would_downgrade_another_package_is_blocked() {
    // uv reaches idna 3.7 only by moving urllib3 back to 1.0.0.
    let baseline = lock("1.15.0", "3.6", "2.0.0");
    let run = prepare_scripted(
        PYPROJECT,
        &baseline,
        &lock("1.15.0", "3.7", "1.0.0"),
        &[advisory("GHSA-direct", "idna", "3.6", "High", &["3.7"])],
        UpdateOptions::default(),
        &[],
        &[],
        &|_| "case \"$*\" in *--locked*) exit 0;; esac\n".into(),
    );
    let proposal = run.result.unwrap();
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    assert_eq!(fix(&proposal, "GHSA-direct").outcome, FixOutcome::Blocked);
    assert!(proposal.changes.is_empty(), "{:?}", proposal.changes);
}

#[test]
fn a_fix_release_that_newer_advisories_affect_is_aimed_past() {
    let raised = lock("1.15.0", "3.8", "2.0.0");
    let run = prepare_scripted(
        PYPROJECT,
        &lock("1.15.0", "3.6", "2.0.0"),
        &lock("1.15.0", "3.7", "2.0.0"),
        &[
            advisory("GHSA-old", "idna", "3.6", "High", &["3.7"]),
            // 3.7, the fix of GHSA-old, has an advisory of its own.
            advisory("GHSA-new", "idna", "3.7", "High", &["3.8"]),
        ],
        UpdateOptions::default(),
        &[],
        &[],
        &|dir| {
            fs::write(dir.join("raised.lock"), &raised).unwrap();
            format!(
                "case \"$*\" in *--locked*) cmp -s '{d}/raised.lock' uv.lock && exit 0;; *'idna==3.8'*) echo \"$*\" >> '{d}/locks'; cp '{d}/raised.lock' uv.lock; exit 0;; esac\n",
                d = dir.display()
            )
        },
    );
    let proposal = run.result.unwrap();
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    assert!(
        run.locks.iter().any(|l| l.contains("idna==3.8")),
        "{:?}",
        run.locks
    );
    let old = fix(&proposal, "GHSA-old");
    assert_eq!(old.outcome, FixOutcome::Fixed);
    assert_eq!(old.fix_version.as_deref(), Some("3.8"));
    assert!(proposal
        .validation
        .iter()
        .any(|v| v.contains("aiming past it")));
}

/// `lock` without the edge from idna to urllib3, so no declared dependency
/// is known to reach it.
fn unreached(lock: String) -> String {
    lock.replace("dependencies = [{ name = \"urllib3\" }]\n", "")
}

#[test]
fn a_transitive_package_with_an_unknown_path_still_moves_and_says_so() {
    let run = prepare(
        &unreached(lock("1.15.0", "3.7", "2.0.0")),
        &unreached(lock("1.15.0", "3.7", "2.0.0")),
        &[advisory(
            "GHSA-transitive",
            "urllib3",
            "2.0.0",
            "High",
            &["2.0.7"],
        )],
        UpdateOptions::default(),
    );
    let proposal = run.result.unwrap();
    assert!(run.locks[0].contains("--upgrade-package urllib3==2.0.7"));
    assert!(
        proposal
            .validation
            .iter()
            .any(|n| n.contains("urllib3") && n.contains("dependency path is unknown")),
        "{:?}",
        proposal.validation
    );
    let transitive = fix(&proposal, "GHSA-transitive");
    assert_eq!(transitive.outcome, FixOutcome::Blocked);
    assert!(transitive.paths.is_empty());
    assert!(
        transitive.reason.contains("stays at 2.0.0")
            && transitive.reason.contains("path is unknown"),
        "{}",
        transitive.reason
    );
}

#[test]
fn findings_below_the_minimum_severity_are_left_alone() {
    let run = prepare(
        &lock("1.15.0", "3.6", "2.0.0"),
        &lock("1.15.0", "3.7", "2.0.0"),
        &[advisory("GHSA-idna", "idna", "3.6", "Low", &["3.7"])],
        UpdateOptions {
            min_severity: Some("medium".into()),
            ..Default::default()
        },
    );
    let proposal = run.result.unwrap();
    assert!(run.locks.is_empty(), "{:?}", run.locks);
    assert!(proposal.security.is_empty(), "{:?}", proposal.security);
    assert!(proposal
        .validation
        .iter()
        .any(|n| n.contains("no actionable findings")));
}

#[test]
fn newest_moves_by_name_and_untargeted_moves_are_listed() {
    let run = prepare(
        &lock("1.15.0", "3.6", "2.0.0"),
        &lock("1.15.0", "3.10", "2.2.0"),
        &[advisory("GHSA-idna", "idna", "3.6", "High", &["3.7"])],
        UpdateOptions {
            security_target: SecurityTarget::Newest,
            ..Default::default()
        },
    );
    let proposal = run.result.unwrap();
    let first = &run.locks[0];
    assert!(
        first.contains("--upgrade-package idna") && !first.contains("idna=="),
        "{first}"
    );
    assert_eq!(fix(&proposal, "GHSA-idna").outcome, FixOutcome::Fixed);
    let note = proposal
        .validation
        .iter()
        .find(|n| n.contains("moved without being targeted"))
        .expect("untargeted moves are listed");
    assert!(note.contains("urllib3 2.0.0 -> 2.2.0"), "{note}");
}

#[test]
fn patch_aims_at_the_newest_release_in_the_fix_line_the_index_publishes() {
    let run = prepare_with_releases(
        PYPROJECT,
        &lock("1.15.0", "3.6", "2.0.0"),
        &lock("1.15.0", "3.7.1", "2.0.0"),
        &[advisory("GHSA-idna", "idna", "3.6", "High", &["3.7"])],
        UpdateOptions {
            security_target: SecurityTarget::Patch,
            ..Default::default()
        },
        &[("idna", &["3.6", "3.7", "3.7.1", "3.8"])],
        &[],
    );
    let proposal = run.result.unwrap();
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    let first = &run.locks[0];
    assert!(first.contains("--upgrade-package idna==3.7.1"), "{first}");
    assert_eq!(
        fix(&proposal, "GHSA-idna").fix_version.as_deref(),
        Some("3.7.1")
    );
    assert!(
        !proposal
            .validation
            .iter()
            .any(|n| n.contains("no release data")),
        "{:?}",
        proposal.validation
    );
}

#[test]
fn patch_aims_at_the_newest_release_in_the_fix_line_the_declaration_admits() {
    let run = prepare_with_releases(
        &PYPROJECT.replace("\"idna\"", "\"idna<3.7.5\""),
        &lock("1.15.0", "3.6", "2.0.0"),
        &lock("1.15.0", "3.7.4", "2.0.0"),
        &[advisory("GHSA-idna", "idna", "3.6", "High", &["3.7"])],
        UpdateOptions {
            security_target: SecurityTarget::Patch,
            ..Default::default()
        },
        &[("idna", &["3.7", "3.7.4", "3.7.9"])],
        &[],
    );
    let proposal = run.result.unwrap();
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    assert!(
        run.locks[0].contains("--upgrade-package idna==3.7.4"),
        "{:?}",
        run.locks
    );
    assert_eq!(fix(&proposal, "GHSA-idna").outcome, FixOutcome::Fixed);
    // No blocked fix: only the usual suggestion that the cap excludes 3.7.9.
    assert!(
        proposal
            .suggestions
            .iter()
            .all(|s| !s.reason.contains("GHSA-idna")),
        "{:?}",
        proposal.suggestions
    );
}

#[test]
fn a_failed_release_listing_is_reported_and_patch_aims_at_the_lowest_fix() {
    let run = prepare_with_releases(
        PYPROJECT,
        &lock("1.15.0", "3.6", "2.0.0"),
        &lock("1.15.0", "3.7", "2.0.0"),
        &[advisory("GHSA-idna", "idna", "3.6", "High", &["3.7"])],
        UpdateOptions {
            security_target: SecurityTarget::Patch,
            ..Default::default()
        },
        &[("idna", &["3.7", "3.7.1"])],
        &["idna"],
    );
    let proposal = run.result.unwrap();
    assert!(
        run.locks[0].ends_with("--upgrade-package idna==3.7"),
        "{:?}",
        run.locks
    );
    assert!(
        proposal
            .validation
            .iter()
            .any(|n| n.contains("idna") && n.contains("release listing failed")),
        "{:?}",
        proposal.validation
    );
}

#[test]
fn patch_without_release_data_aims_at_the_lowest_fix_and_says_so() {
    let run = prepare(
        &lock("1.15.0", "3.6", "2.0.0"),
        &lock("1.15.0", "3.7", "2.0.0"),
        &[advisory("GHSA-idna", "idna", "3.6", "High", &["3.7"])],
        UpdateOptions {
            security_target: SecurityTarget::Patch,
            ..Default::default()
        },
    );
    let proposal = run.result.unwrap();
    assert!(run.locks[0].contains("--upgrade-package idna==3.7"));
    assert!(
        proposal
            .validation
            .iter()
            .any(|n| n.contains("idna") && n.contains("no release data")),
        "{:?}",
        proposal.validation
    );
}

#[test]
fn a_security_update_that_introduces_a_finding_fails() {
    let run = prepare(
        &lock("1.15.0", "3.6", "2.0.0"),
        &lock("1.15.0", "3.7", "2.0.0"),
        &[
            advisory("GHSA-idna", "idna", "3.6", "High", &["3.7"]),
            advisory("GHSA-new", "idna", "3.7", "Low", &["3.8"]),
        ],
        UpdateOptions::default(),
    );
    let proposal = run.result.unwrap();
    assert_eq!(proposal.failures.len(), 1, "{:?}", proposal.validation);
    assert_eq!(proposal.failures[0].code, 4);
    assert!(proposal.failures[0].message.contains("GHSA-new"));
}

#[test]
fn security_updates_never_change_constraints_or_pick_packages() {
    for options in [
        UpdateOptions {
            upgrade: true,
            ..Default::default()
        },
        UpdateOptions {
            packages: vec!["idna".into()],
            ..Default::default()
        },
    ] {
        let run = prepare(
            &lock("1.15.0", "3.6", "2.0.0"),
            &lock("1.15.0", "3.7", "2.0.0"),
            &[],
            options,
        );
        assert!(matches!(run.result, Err(Error::Invalid(_))));
    }
}

#[test]
fn accepting_a_requirement_that_admits_the_fix_lets_it_through() {
    let run = prepare(
        &lock("1.15.0", "3.7", "2.0.0"),
        &lock("1.16.0", "3.7", "2.0.0"),
        &[advisory("GHSA-six", "six", "1.15.0", "High", &["1.16.0"])],
        UpdateOptions {
            accept: vec!["six===1.16.0".into()],
            ..Default::default()
        },
    );
    let proposal = run.result.unwrap();
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    assert!(
        run.locks[0].contains("--upgrade-package six==1.16.0"),
        "{:?}",
        run.locks
    );
    assert_eq!(fix(&proposal, "GHSA-six").outcome, FixOutcome::Fixed);
    let manifest = proposal
        .changes
        .iter()
        .find(|c| c.path.ends_with("pyproject.toml"))
        .expect("the accepted requirement is written");
    assert!(manifest.after.contains("six==1.16.0"), "{}", manifest.after);
}

#[test]
fn policy_gates_still_apply_when_nothing_can_move() {
    let run = prepare(
        &lock("1.15.0", "3.7", "2.0.0"),
        &lock("1.15.0", "3.7", "2.0.0"),
        &[advisory("GHSA-open", "idna", "3.7", "High", &[])],
        UpdateOptions {
            fail_on: Some("high".into()),
            ..Default::default()
        },
    );
    let proposal = run.result.unwrap();
    assert_eq!(proposal.failures.len(), 1, "{:?}", proposal.validation);
    assert_eq!(proposal.failures[0].code, 4);
    assert_eq!(proposal.exit_code(true), 4);
}

#[test]
fn a_suppressed_finding_the_update_introduces_does_not_fail_it() {
    let run = prepare(
        &lock("1.15.0", "3.6", "2.0.0"),
        &lock("1.15.0", "3.7", "2.0.0"),
        &[
            advisory("GHSA-idna", "idna", "3.6", "High", &["3.7"]),
            advisory("GHSA-new", "idna", "3.7", "Low", &["3.8"]),
        ],
        UpdateOptions {
            suppressions: vec![depsmith_core::scan::Suppression {
                id: "GHSA-new".into(),
                package: Some("pypi:idna".into()),
                target: None,
                reason: "reviewed".into(),
                expires: None,
            }],
            ..Default::default()
        },
    );
    let proposal = run.result.unwrap();
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    assert_eq!(fix(&proposal, "GHSA-idna").outcome, FixOutcome::Fixed);
}

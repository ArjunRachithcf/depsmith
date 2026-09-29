//! Conformance checks every adapter must pass, for adapter authors' tests.
//!
//! Call [`check`] with a way to build the adapter and a small repository
//! [`Fixture`] containing one of its targets. The checks exercise only the
//! adapter seam and the engine, never the adapter's native tool, so they run
//! offline.
use crate::{
    adapter::{Adapter, AdapterSpec, Support},
    constraints::Edit,
    ecosystem, working_tree, Engine, Error, UpdateOptions,
};
use std::{collections::BTreeSet, fs, path::Path};

/// A repository containing one target of the adapter under test.
#[derive(Debug, Clone)]
pub struct Fixture {
    /// Files to create, as (path relative to the root, content).
    pub files: Vec<(String, String)>,
    /// Identifier of the target the files define, such as `pixi:pixi.toml`.
    pub target: String,
    /// Direct dependencies as (requested name, declared spelling) that
    /// package selection must resolve; ignored without package selection.
    pub declared: Vec<(String, String)>,
}

/// Run every conformance check against the adapter built by `make`.
///
/// # Errors
///
/// Returns every violated expectation, one message each.
pub fn check(make: &dyn Fn() -> Box<dyn Adapter>, fixture: &Fixture) -> Result<(), Vec<String>> {
    let mut problems = vec![];
    let spec = make().spec();
    check_spec(&spec, &mut problems);
    let root = match write_fixture(fixture) {
        Ok(root) => root,
        Err(error) => return Err(vec![format!("cannot write fixture: {error}")]),
    };
    check_discovery_and_staging(make, &spec, fixture, root.path(), &mut problems);
    check_selection(make, &spec, fixture, root.path(), &mut problems);
    check_declarations(make, &spec, fixture, &mut problems);
    check_capability_enforcement(make, &spec, fixture, root.path(), &mut problems);
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

fn write_fixture(fixture: &Fixture) -> std::io::Result<tempfile::TempDir> {
    let root = tempfile::tempdir()?;
    for (path, content) in &fixture.files {
        let path = root.path().join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, content)?;
    }
    Ok(root)
}

fn check_spec(spec: &AdapterSpec, problems: &mut Vec<String>) {
    match serde_json::to_string(spec).map(|json| serde_json::from_str::<AdapterSpec>(&json)) {
        Ok(Ok(round_trip)) if round_trip == *spec => {}
        _ => problems.push("spec does not round-trip through serde".into()),
    }
    if spec.manager.is_empty() || spec.manager.contains([':', ' ']) {
        problems.push(format!(
            "manager {:?} must be non-empty without ':' or spaces",
            spec.manager
        ));
    }
    if spec.patterns.is_empty() {
        problems.push("spec declares no discovery patterns".into());
    }
    let mut names = BTreeSet::new();
    for tool in &spec.tools {
        if tool.name.is_empty() || tool.default.is_empty() || !names.insert(&tool.name) {
            problems.push(format!(
                "tool {:?} needs a unique name and a default",
                tool.name
            ));
        }
    }
}

fn check_discovery_and_staging(
    make: &dyn Fn() -> Box<dyn Adapter>,
    spec: &AdapterSpec,
    fixture: &Fixture,
    root: &Path,
    problems: &mut Vec<String>,
) {
    let engine = Engine::new(vec![make()]);
    let targets = match engine.discover(root) {
        Ok(targets) => targets,
        Err(error) => return problems.push(format!("discovery failed: {error}")),
    };
    let Some(target) = targets.iter().find(|t| t.id == fixture.target) else {
        return problems.push(format!(
            "discovery did not find {}; found {:?}",
            fixture.target,
            targets.iter().map(|t| &t.id).collect::<Vec<_>>()
        ));
    };
    let name = target
        .manifest
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let layout = working_tree::Layout::new([spec]);
    for rule in &spec.managed {
        if !rule
            .manifests
            .iter()
            .any(|p| working_tree::matches_pattern(p, name))
        {
            continue;
        }
        for input in &rule.inputs {
            let path = target
                .manifest
                .parent()
                .unwrap_or(Path::new(""))
                .join(input);
            if !root.join(&path).exists() {
                if let Some(parent) = root.join(&path).parent() {
                    let _ = fs::create_dir_all(parent);
                }
                let _ = fs::write(root.join(&path), "conformance\n");
            }
            match working_tree::fingerprint(root, &layout) {
                Ok(inputs) if inputs.contains_key(&path) => {}
                Ok(_) => problems.push(format!(
                    "managed input {} is not staged or fingerprinted",
                    path.display()
                )),
                Err(error) => problems.push(format!("fingerprinting failed: {error}")),
            }
        }
    }
}

fn check_selection(
    make: &dyn Fn() -> Box<dyn Adapter>,
    spec: &AdapterSpec,
    fixture: &Fixture,
    root: &Path,
    problems: &mut Vec<String>,
) {
    if spec.capabilities.package_selection != Support::Supported {
        return;
    }
    let adapter = make();
    let Ok(targets) = Engine::new(vec![make()]).discover(root) else {
        return;
    };
    let Some(target) = targets.iter().find(|t| t.id == fixture.target) else {
        return;
    };
    for (requested, declared) in &fixture.declared {
        let asked = [requested.clone(), "depsmith-conformance-undeclared".into()];
        match adapter.select(root, target, &asked) {
            Ok(selected) => {
                if selected.get(requested) != Some(declared) {
                    problems.push(format!(
                        "select({requested}) returned {:?}, expected the declared spelling {declared}",
                        selected.get(requested)
                    ));
                }
                if selected.contains_key(&asked[1]) {
                    problems.push("select matched an undeclared package".into());
                }
            }
            Err(error) => problems.push(format!("select({requested}) failed: {error}")),
        }
    }
}

/// Declarations must name existing files and round-trip through rewrite: an
/// identity edit leaves every file byte-identical, and a new requirement is
/// what the adapter then declares at the same place. Required of adapters
/// that support `--accept`; the fixture must declare a versioned dependency.
fn check_declarations(
    make: &dyn Fn() -> Box<dyn Adapter>,
    spec: &AdapterSpec,
    fixture: &Fixture,
    problems: &mut Vec<String>,
) {
    if spec.capabilities.suggestion_acceptance != Support::Supported {
        return;
    }
    let adapter = make();
    let fresh = || -> Result<_, String> {
        let root = write_fixture(fixture).map_err(|e| format!("cannot write fixture: {e}"))?;
        let targets = Engine::new(vec![make()])
            .discover(root.path())
            .map_err(|e| format!("discovery failed: {e}"))?;
        let target = targets
            .into_iter()
            .find(|t| t.id == fixture.target)
            .ok_or_else(|| format!("discovery did not find {}", fixture.target))?;
        let declarations = adapter
            .declarations(root.path(), &target)
            .map_err(|e| format!("declarations failed: {e}"))?;
        Ok((root, target, declarations))
    };
    let snapshot = |root: &Path| -> Vec<(String, Option<Vec<u8>>)> {
        fixture
            .files
            .iter()
            .map(|(path, _)| (path.clone(), fs::read(root.join(path)).ok()))
            .collect()
    };
    let (root, target, declarations) = match fresh() {
        Ok(found) => found,
        Err(problem) => return problems.push(problem),
    };
    for declaration in &declarations {
        if !root.path().join(&declaration.file).is_file() {
            problems.push(format!(
                "declaration of {} names a missing file {}",
                declaration.package,
                declaration.file.display()
            ));
        }
    }
    let versioned: Vec<_> = declarations
        .iter()
        .filter(|d| !d.requirement.is_empty())
        .collect();
    let Some(first) = versioned.first() else {
        return problems
            .push("--accept is supported but the fixture declares no versioned dependency".into());
    };
    let before = snapshot(root.path());
    let identity: Vec<Edit> = versioned
        .iter()
        .map(|d| Edit {
            declaration: (*d).clone(),
            requirement: d.requirement.clone(),
        })
        .collect();
    match adapter.rewrite(root.path(), &target, &identity) {
        Ok(()) if snapshot(root.path()) == before => {}
        Ok(()) => problems.push("rewriting declarations unchanged changed the files".into()),
        Err(error) => problems.push(format!("rewrite failed: {error}")),
    }
    let (root, target, _) = match fresh() {
        Ok(found) => found,
        Err(problem) => return problems.push(problem),
    };
    let requirement = ecosystem::scheme(&first.ecosystem)
        .and_then(|scheme| scheme.restyle(&first.requirement, "999999"))
        .unwrap_or_else(|| "==999999".into());
    let edit = Edit {
        declaration: (*first).clone(),
        requirement: requirement.clone(),
    };
    if let Err(error) = adapter.rewrite(root.path(), &target, &[edit]) {
        return problems.push(format!("rewrite failed: {error}"));
    }
    let mut expected = declarations.clone();
    let index = declarations.iter().position(|d| d == *first).unwrap();
    expected[index].requirement = requirement;
    match adapter.declarations(root.path(), &target) {
        Ok(after) if after == expected => {}
        Ok(after) => problems.push(format!(
            "after rewriting {} to {}, declarations were {after:?}; expected {expected:?}",
            first.package, expected[index].requirement
        )),
        Err(error) => problems.push(format!("declarations failed after rewrite: {error}")),
    }
}

fn check_capability_enforcement(
    make: &dyn Fn() -> Box<dyn Adapter>,
    spec: &AdapterSpec,
    fixture: &Fixture,
    root: &Path,
    problems: &mut Vec<String>,
) {
    let caps = &spec.capabilities;
    let package = fixture
        .declared
        .first()
        .map_or_else(|| "depsmith-conformance".to_owned(), |(_, d)| d.clone());
    let missing = "depsmith-conformance-missing-tool".to_string();
    let base = UpdateOptions {
        tools: spec
            .tools
            .iter()
            .map(|t| (t.name.clone(), missing.clone()))
            .collect(),
        pixi: missing.clone(),
        grype: missing,
        ..Default::default()
    };
    // Requests that must be rejected before the adapter runs: unsupported
    // behaviours, and a cooldown the adapter cannot enforce.
    let requests: Vec<(&str, &Support, bool, UpdateOptions)> = vec![
        (
            "package selection",
            &caps.package_selection,
            false,
            UpdateOptions {
                packages: vec![package.clone()],
                ..base.clone()
            },
        ),
        (
            "--upgrade",
            &caps.constraint_changes,
            false,
            UpdateOptions {
                upgrade: true,
                packages: vec![package.clone()],
                ..base.clone()
            },
        ),
        (
            "--accept",
            &caps.suggestion_acceptance,
            false,
            UpdateOptions {
                accept: vec![package.clone()],
                ..base.clone()
            },
        ),
        (
            "--refresh-git",
            &caps.git_refresh,
            false,
            UpdateOptions {
                refresh_git: true,
                ..base.clone()
            },
        ),
        (
            "--cooldown-days",
            &caps.cooldown,
            true,
            UpdateOptions {
                cooldown_days: Some(1),
                ..base.clone()
            },
        ),
        (
            "--install",
            &caps.install_validation,
            false,
            UpdateOptions {
                install: true,
                ..base.clone()
            },
        ),
    ];
    for (name, support, restrictive, options) in requests {
        let must_reject = matches!(support, Support::Unsupported(_))
            || (restrictive && *support == Support::NotApplicable);
        if !must_reject {
            continue;
        }
        let engine = Engine::new(vec![make()]);
        match engine.prepare(root, std::slice::from_ref(&fixture.target), options) {
            Err(Error::Invalid(message)) if message.contains(name) => {}
            Err(error) => problems.push(format!(
                "{name}: rejected with an unexpected error: {error}"
            )),
            Ok(_) => problems.push(format!(
                "{name} is {support:?} but preparing was not rejected before the adapter ran"
            )),
        }
    }
}

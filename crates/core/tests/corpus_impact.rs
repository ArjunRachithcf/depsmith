//! The corpus harness's impact driver (`scripts/corpus/run.py`): after an
//! applied update in a real repository, compares the target's lock at `HEAD`
//! with the working tree and walks the change to the target's usage sites.
//! Ignored by default; the harness sets `CORPUS_ROOT` and `CORPUS_TARGET`
//! (`manager:path`) and runs it with `--ignored --nocapture`.
use depsmith_core::{
    adapter::Adapter,
    cargo::Cargo,
    conda::Conda,
    impact::{affected, sides, usages, Comparison, ImportNames, Lock, RUNTIME_BARRIERS},
    npm::Npm,
    pixi::Pixi,
    uv::Uv,
    Target,
};
use std::path::{Path, PathBuf};

#[test]
#[ignore = "driven by the corpus harness"]
fn impact_of_the_applied_update() {
    let root = PathBuf::from(std::env::var("CORPUS_ROOT").expect("CORPUS_ROOT"));
    let id = std::env::var("CORPUS_TARGET").expect("CORPUS_TARGET");
    let (manager, manifest) = id.split_once(':').expect("manager:path");
    let target = Target {
        id: id.clone(),
        manager: manager.into(),
        manifest: manifest.into(),
    };
    let (adapter, barriers): (Box<dyn Adapter>, &[&str]) = match manager {
        "pixi" => (Box::new(Pixi), RUNTIME_BARRIERS),
        "conda" => (Box::new(Conda), RUNTIME_BARRIERS),
        "uv" => (Box::new(Uv::default()), &[]),
        "npm" => (Box::new(Npm::default()), &[]),
        "cargo" => (Box::new(Cargo::default()), &[]),
        other => panic!("no impact driver for {other}"),
    };
    let comparison = Comparison::Worktree {
        base: Some("HEAD".into()),
    };
    let sides = sides(&root, adapter.as_ref(), &target, &comparison).expect("sides");
    let read = |dir: &Path| Lock::read(adapter.as_ref(), dir, &target).expect("lock");
    let (before, after) = (read(sides.before()), read(sides.after()));
    let declarations = adapter
        .declarations(sides.after(), &target)
        .expect("declarations");
    let result = affected(&before, &after, &declarations, barriers);
    let names = ImportNames::read(sides.after(), &target);
    let found = usages(&root, &target, &result, &names).expect("usages");
    println!(
        "CORPUS_IMPACT changes={} packages={} full_run={} sites={} files={} warnings={}",
        result.changes.len(),
        result.packages.len(),
        result.full_run.len(),
        found.sites.len(),
        found.files.len(),
        found.warnings.len()
    );
}

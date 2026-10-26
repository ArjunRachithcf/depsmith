//! The stage: what adapters see of the repository they resolve.
use depsmith_core::{
    adapter::{Adapter, AdapterSpec, Candidate},
    Engine, Error, Result, Target, UpdateOptions,
};
use std::{fs, path::Path};

/// Reports the name of the directory it is asked to resolve in.
struct Probe;

impl Adapter for Probe {
    fn spec(&self) -> AdapterSpec {
        AdapterSpec::new("probe", &["probe.txt"])
    }
    fn detects(&self, _: &Path, _: &str) -> bool {
        true
    }
    fn prepare(&self, stage: &Path, _: &Target, _: &UpdateOptions) -> Result<Candidate> {
        Err(Error::Invalid(format!(
            "stage directory: {}",
            stage.file_name().unwrap().to_string_lossy()
        )))
    }
}

/// Package managers name a project without a declared name after its
/// directory.
#[test]
fn the_stage_directory_has_the_repository_directory_name() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("shop");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("probe.txt"), "").unwrap();
    let proposal = Engine::new(vec![Box::new(Probe)])
        .prepare(&root, &["probe:probe.txt".into()], UpdateOptions::default())
        .unwrap();
    assert_eq!(
        proposal.failures[0].message,
        "invalid request: stage directory: shop"
    );
}

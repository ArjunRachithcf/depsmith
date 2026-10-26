//! Targets of different adapters that share a file, such as a pyproject.toml
//! owned by both Pixi and uv: the same edit is one change, different edits
//! conflict.
use depsmith_core::{
    adapter::{Adapter, AdapterSpec, Candidate},
    Engine, Error, Result, Target, UpdateOptions,
};
use std::{fs, path::Path};

/// Writes `text` to `shared.txt` beside a `NAME.txt` manifest.
struct Writer {
    name: &'static str,
    manifest: &'static str,
    text: &'static str,
}

impl Adapter for Writer {
    fn spec(&self) -> AdapterSpec {
        AdapterSpec::new(self.name, &[self.manifest])
    }
    fn detects(&self, _: &Path, _: &str) -> bool {
        true
    }
    fn prepare(&self, stage: &Path, _: &Target, _: &UpdateOptions) -> Result<Candidate> {
        fs::write(stage.join("shared.txt"), self.text)?;
        Ok(Candidate {
            files: vec!["shared.txt".into()],
            ..Default::default()
        })
    }
}

fn prepare(first: &'static str, second: &'static str) -> Result<depsmith_core::Proposal> {
    let root = tempfile::tempdir().unwrap();
    for name in ["a.txt", "b.txt", "shared.txt"] {
        fs::write(root.path().join(name), "old\n").unwrap();
    }
    Engine::new(vec![
        Box::new(Writer {
            name: "a",
            manifest: "a.txt",
            text: first,
        }),
        Box::new(Writer {
            name: "b",
            manifest: "b.txt",
            text: second,
        }),
    ])
    .prepare(
        root.path(),
        &["a:a.txt".into(), "b:b.txt".into()],
        UpdateOptions::default(),
    )
}

#[test]
fn the_same_edit_of_a_shared_file_is_one_change() {
    let proposal = prepare("new\n", "new\n").unwrap();
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    let paths: Vec<_> = proposal.changes.iter().map(|c| c.path.clone()).collect();
    assert_eq!(paths, [Path::new("shared.txt")]);
}

#[test]
fn different_edits_of_a_shared_file_conflict() {
    let error = prepare("new\n", "other\n").unwrap_err();
    assert!(
        matches!(&error, Error::Invalid(m) if m.contains("overlapping file changes")),
        "{error}"
    );
}

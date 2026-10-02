//! Adding a package manager: a new adapter is picked up through its spec
//! alone, with no changes to the engine, options, doctor or staging.
use depsmith_core::{
    adapter::{Adapter, AdapterSpec, Candidate, Capabilities, ManagedFiles, Support, ToolSpec},
    apply, conformance, Engine, Error, Result, Target, UpdateOptions,
};
use std::{collections::BTreeMap, fs, path::Path};

/// A package manager depsmith has never heard of: `demo.json` manifests with a
/// `demo.lock` next to them, resolved by a `demo-tool` executable.
struct Demo;

impl Adapter for Demo {
    fn spec(&self) -> AdapterSpec {
        AdapterSpec {
            manager: "demo".into(),
            ecosystems: vec!["demo".into()],
            patterns: vec!["demo.json".into()],
            managed: vec![ManagedFiles {
                manifests: vec!["demo.json".into()],
                inputs: vec!["demo.lock".into()],
            }],
            skip_dirs: vec![".demo-env".into()],
            tools: vec![ToolSpec {
                name: "demo-tool".into(),
                default: "demo-tool".into(),
                tested_versions: vec!["1.0.0".into()],
                downloads: vec![],
            }],
            capabilities: Capabilities {
                package_selection: Support::Supported,
                ..Capabilities::undeclared()
            },
        }
    }
    fn detects(&self, _: &Path, content: &str) -> bool {
        content.contains("\"demo\"")
    }
    fn select(
        &self,
        root: &Path,
        target: &Target,
        requested: &[String],
    ) -> Result<BTreeMap<String, String>> {
        let text = fs::read_to_string(root.join(&target.manifest))?;
        Ok(requested
            .iter()
            .filter_map(|r| {
                ["Alpha", "beta"]
                    .into_iter()
                    .find(|d| text.contains(d) && d.eq_ignore_ascii_case(r))
                    .map(|d| (r.clone(), d.to_owned()))
            })
            .collect())
    }
    fn prepare(&self, stage: &Path, target: &Target, options: &UpdateOptions) -> Result<Candidate> {
        let lock = target.manifest.with_file_name("demo.lock");
        let seen = fs::read_to_string(stage.join(&lock)).unwrap_or_default();
        fs::write(stage.join(&lock), "resolved\n")?;
        Ok(Candidate {
            files: vec![lock],
            validation: vec![
                format!("tool {}", options.tool("demo-tool")),
                format!("lock {}", seen.trim()),
            ],
            ..Default::default()
        })
    }
}

fn repo() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("demo.json"),
        "{\"demo\": [\"Alpha\", \"beta\"]}",
    )
    .unwrap();
    fs::write(root.path().join("demo.lock"), "old\n").unwrap();
    fs::write(root.path().join(".gitignore"), "demo.lock\n.demo-env/\n").unwrap();
    fs::create_dir(root.path().join(".demo-env")).unwrap();
    fs::write(root.path().join(".demo-env/demo.json"), "{\"demo\": []}").unwrap();
    fs::create_dir(root.path().join("bin")).unwrap();
    fs::write(root.path().join("bin/demo.json"), [0xff, 0xfe, 0x00]).unwrap();
    root
}

fn options() -> UpdateOptions {
    UpdateOptions {
        tools: [(
            "demo-tool".to_string(),
            "/opt/demo/bin/demo-tool".to_string(),
        )]
        .into(),
        ..Default::default()
    }
}

#[test]
fn a_new_adapter_is_discovered_through_its_spec() {
    let root = repo();
    let targets = Engine::new(vec![Box::new(Demo)])
        .discover(root.path())
        .unwrap();
    // Not TOML or YAML, a skipped environment directory, and a non-UTF-8 file.
    assert_eq!(
        targets.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
        ["demo:demo.json"]
    );
}

#[test]
fn doctor_reports_a_new_adapters_tool_from_the_tools_map() {
    let report = Engine::new(vec![Box::new(Demo)]).doctor(std::path::Path::new("."), &options());
    let tool = report["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["tool"] == "demo-tool")
        .expect("demo-tool reported");
    assert_eq!(tool["program"], "/opt/demo/bin/demo-tool");
    assert_eq!(tool["status"], "unavailable");
    assert_eq!(tool["tested_versions"][0], "1.0.0");
    let adapter = &report["adapters"][0];
    assert_eq!(adapter["manager"], "demo");
    assert_eq!(adapter["package_selection"]["status"], "supported");
}

#[test]
fn managed_files_are_staged_and_guard_against_stale_proposals() {
    let root = repo();
    let engine = Engine::new(vec![Box::new(Demo)]);
    let proposal = engine
        .prepare(root.path(), &["demo:demo.json".into()], options())
        .unwrap();
    assert!(proposal.failures.is_empty(), "{:?}", proposal.failures);
    // The gitignored lock reached the stage, and the tools map reached the adapter.
    assert!(
        proposal.validation.contains(&"lock old".to_string()),
        "{:?}",
        proposal.validation
    );
    assert!(proposal
        .validation
        .contains(&"tool /opt/demo/bin/demo-tool".to_string()));
    assert_eq!(proposal.changes[0].after, "resolved\n");
    fs::write(root.path().join("demo.lock"), "edited\n").unwrap();
    assert!(matches!(apply(&proposal, false), Err(Error::Stale(_))));
}

#[test]
fn legacy_tool_options_remain_aliases() {
    let options = UpdateOptions {
        pixi: "/legacy/pixi".into(),
        ..Default::default()
    };
    assert_eq!(options.tool("pixi"), "/legacy/pixi");
    let options = UpdateOptions {
        pixi: "/legacy/pixi".into(),
        tools: [("pixi".to_string(), "/new/pixi".to_string())].into(),
        ..Default::default()
    };
    assert_eq!(options.tool("pixi"), "/new/pixi");
    assert_eq!(UpdateOptions::default().tool("anything"), "anything");
}

#[test]
fn every_adapter_passes_the_conformance_suite() {
    let demo = conformance::Fixture {
        files: vec![(
            "demo.json".into(),
            "{\"demo\": [\"Alpha\", \"beta\"]}".into(),
        )],
        target: "demo:demo.json".into(),
        declared: vec![("ALPHA".into(), "Alpha".into())],
    };
    conformance::check(&|| Box::new(Demo) as Box<dyn Adapter>, &demo).unwrap();

    let pixi = conformance::Fixture {
        files: vec![(
            "pixi.toml".into(),
            "[workspace]\nname='a'\nchannels=['conda-forge']\nplatforms=['linux-64']\n\
             [dependencies]\nRuff='*'\n"
                .into(),
        )],
        target: "pixi:pixi.toml".into(),
        declared: vec![("ruff".into(), "Ruff".into())],
    };
    conformance::check(
        &|| Box::new(depsmith_core::pixi::Pixi) as Box<dyn Adapter>,
        &pixi,
    )
    .unwrap();

    let actions = conformance::Fixture {
        files: vec![(
            ".github/workflows/ci.yml".into(),
            "on: push\njobs:\n  b:\n    runs-on: x\n    steps:\n      - uses: Actions/Checkout@v4\n"
                .into(),
        )],
        target: "github-actions:.github/workflows/ci.yml".into(),
        declared: vec![("actions/checkout".into(), "Actions/Checkout".into())],
    };
    conformance::check(
        &|| Box::new(depsmith_core::actions::Actions::default()) as Box<dyn Adapter>,
        &actions,
    )
    .unwrap();
}

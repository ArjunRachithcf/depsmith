//! The Integration workflow's tool pins are the adapters' tested versions, so
//! "tested" means what CI runs and the Latest tools report compares the right
//! versions. Skipped outside a checkout (an sdist has no workflows).
use depsmith_core::Engine;
use std::{collections::BTreeMap, path::Path};

#[test]
fn integration_pins_are_the_tested_versions() {
    let workflow =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows/integration.yml");
    let Ok(text) = std::fs::read_to_string(&workflow) else {
        return;
    };
    let tested: BTreeMap<String, String> = Engine::default()
        .specs()
        .into_iter()
        .flat_map(|s| s.tools)
        .chain([depsmith_core::scan::scanner_tool()])
        .map(|t| (t.name, t.tested_versions[0].clone()))
        .collect();
    let pin = |variable: &str| -> String {
        let line = text
            .lines()
            .find(|l| l.trim_start().starts_with(&format!("{variable}:")))
            .unwrap_or_else(|| panic!("no {variable} in {}", workflow.display()));
        // `${{ ... && 'PIN' || '' }}` or `${{ ... && 'latest' || 'PIN' }}`
        let quoted: Vec<&str> = line.split('\'').skip(1).step_by(2).collect();
        quoted
            .iter()
            .rev()
            .find(|q| !q.is_empty() && **q != "latest" && !tested.contains_key(**q))
            .unwrap_or_else(|| panic!("no pin in {line}"))
            .trim_start_matches("==")
            .trim_start_matches('v')
            .to_owned()
    };
    for (variable, tool) in [
        ("PIXI_VERSION", "pixi"),
        ("UV_VERSION", "uv"),
        ("GRYPE_VERSION", "grype"),
        ("CONDA_LOCK_PIN", "conda-lock"),
        ("MICROMAMBA_PIN", "conda"),
    ] {
        assert_eq!(pin(variable), tested[tool], "{variable} pins {tool}");
    }
}

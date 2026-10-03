//! `.github/tool-versions.json` pins the native tools Integration installs;
//! each pin must be the adapter's tested version, so "tested" means what CI
//! runs and the Latest tools report compares the right versions. Skipped
//! outside a checkout (an sdist has no `.github`).
use depsmith_core::Engine;
use std::{collections::BTreeMap, path::Path};

#[test]
fn integration_pins_are_the_tested_versions() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/tool-versions.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let pins: BTreeMap<String, String> = serde_json::from_str(&text).unwrap();
    let tested: BTreeMap<String, String> = Engine::default()
        .specs()
        .into_iter()
        .flat_map(|s| s.tools)
        .chain([depsmith_core::scan::scanner_tool()])
        .filter_map(|t| Some((t.name.clone(), t.pinned_version()?.to_owned())))
        .collect();
    assert_eq!(
        pins.keys().collect::<Vec<_>>(),
        ["conda", "conda-lock", "grype", "node", "pixi", "uv"],
        "{}",
        path.display()
    );
    // Node.js is not a tool of its own: npm comes with this release.
    assert_eq!(pins["node"], depsmith_core::provision::NODE_VERSION);
    for (tool, pin) in pins.iter().filter(|(tool, _)| *tool != "node") {
        assert_eq!(Some(pin), tested.get(tool), "{tool} in {}", path.display());
    }
}

/// The published crates carry the MIT license text, copied from the root.
#[test]
fn published_crates_carry_the_root_license() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let Ok(license) = std::fs::read_to_string(root.join("LICENSE")) else {
        return;
    };
    for krate in ["crates/core", "crates/cli"] {
        let copy = std::fs::read_to_string(root.join(krate).join("LICENSE")).unwrap();
        assert_eq!(copy, license, "{krate}/LICENSE differs from LICENSE");
    }
}

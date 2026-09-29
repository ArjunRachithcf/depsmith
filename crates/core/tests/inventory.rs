//! Reading inventories from Pixi lockfiles.
use depsmith_core::inventory::pixi_inventory;
#[test]
fn local_editables_with_dynamic_versions_are_retained_as_unknown() {
    let packages = pixi_inventory("version: 7\npackages:\n- pypi: ./\n  name: demo\n- conda: https://conda.anaconda.org/conda-forge/linux-64/lib-blas-1.2.3-h123_0.conda\n").unwrap();
    assert_eq!(packages[0].name, "demo");
    assert_eq!(packages[0].version, "unknown");
    assert_eq!(packages[1].name, "lib-blas");
    assert_eq!(packages[1].version, "1.2.3");
}

#[test]
fn shared_wheels_are_reported_for_each_locked_target_platform() {
    let packages = pixi_inventory("version: 7\nenvironments:\n  default:\n    packages:\n      linux-64:\n      - pypi: https://files.pythonhosted.org/demo.whl\n      win-64:\n      - pypi: https://files.pythonhosted.org/demo.whl\npackages:\n- pypi: https://files.pythonhosted.org/demo.whl\n  name: demo\n  version: '1.0'\n").unwrap();
    assert_eq!(
        packages
            .iter()
            .map(|p| p.platform.as_str())
            .collect::<Vec<_>>(),
        ["linux-64", "win-64"]
    );
}

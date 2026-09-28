use depsmith_core as core;
use pyo3::{create_exception, exceptions::PyRuntimeError, prelude::*};
use std::path::Path;

create_exception!(_native, ConfigurationError, PyRuntimeError);
create_exception!(_native, OperationError, PyRuntimeError);
create_exception!(_native, StaleProposalError, PyRuntimeError);
create_exception!(_native, PolicyError, PyRuntimeError);
fn error(error: core::Error) -> PyErr {
    let message = error.to_string();
    match error {
        core::Error::Invalid(_) => ConfigurationError::new_err(message),
        core::Error::Stale(_) => StaleProposalError::new_err(message),
        core::Error::Policy(_) => PolicyError::new_err(message),
        _ => OperationError::new_err(message),
    }
}
fn config(root: &str, options: &str) -> core::Result<core::config::Config> {
    let overrides =
        serde_json::from_str(options).map_err(|e| core::Error::Invalid(e.to_string()))?;
    core::config::settings(Path::new(root), &overrides)
}
#[pyclass]
struct NativeProposal {
    inner: core::Proposal,
}
#[pymethods]
impl NativeProposal {
    fn report(&self) -> String {
        serde_json::to_string(&self.inner).unwrap()
    }
    #[pyo3(signature = (allow_partial=false))]
    fn apply(&self, py: Python<'_>, allow_partial: bool) -> PyResult<String> {
        py.allow_threads(|| core::apply(&self.inner, allow_partial))
            .map(|r| serde_json::to_string(&r).unwrap())
            .map_err(error)
    }
}
#[pyfunction]
fn discover_json(py: Python<'_>, root: String) -> PyResult<String> {
    py.allow_threads(|| core::discover(Path::new(&root)))
        .map(|r| serde_json::to_string(&r).unwrap())
        .map_err(error)
}
#[pyfunction]
fn prepare(
    py: Python<'_>,
    root: String,
    targets: Vec<String>,
    options: String,
) -> PyResult<NativeProposal> {
    py.allow_threads(|| {
        let settings = config(&root, &options)?;
        let selected = if targets.is_empty() {
            settings.targets
        } else {
            targets
        };
        core::Engine::default().prepare(Path::new(&root), &selected, settings.options)
    })
    .map(|inner| NativeProposal { inner })
    .map_err(error)
}
#[pyfunction]
fn scan_json(
    py: Python<'_>,
    root: String,
    targets: Vec<String>,
    options: String,
) -> PyResult<String> {
    py.allow_threads(|| {
        let settings = config(&root, &options)?;
        let selected = if targets.is_empty() {
            settings.targets
        } else {
            targets
        };
        core::scan_existing(Path::new(&root), &selected, &settings.options)
    })
    .map(|r| serde_json::to_string(&r).unwrap())
    .map_err(error)
}
#[pyfunction]
fn doctor_json(py: Python<'_>, root: String, options: String) -> PyResult<String> {
    py.allow_threads(|| config(&root, &options).map(|settings| core::doctor(&settings.options)))
        .map(|r| serde_json::to_string(&r).unwrap())
        .map_err(error)
}
#[pyfunction]
fn recover_json(py: Python<'_>, root: String) -> PyResult<String> {
    py.allow_threads(|| core::recover(Path::new(&root)))
        .map(|r| serde_json::to_string(&r).unwrap())
        .map_err(error)
}
#[pyfunction]
fn cli(py: Python<'_>, args: Vec<String>) -> u8 {
    py.allow_threads(|| depsmith_cli::run_from(args))
}
#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<NativeProposal>()?;
    m.add_function(wrap_pyfunction!(discover_json, m)?)?;
    m.add_function(wrap_pyfunction!(prepare, m)?)?;
    m.add_function(wrap_pyfunction!(scan_json, m)?)?;
    m.add_function(wrap_pyfunction!(doctor_json, m)?)?;
    m.add_function(wrap_pyfunction!(recover_json, m)?)?;
    m.add_function(wrap_pyfunction!(cli, m)?)?;
    m.add(
        "ConfigurationError",
        m.py().get_type::<ConfigurationError>(),
    )?;
    m.add("OperationError", m.py().get_type::<OperationError>())?;
    m.add(
        "StaleProposalError",
        m.py().get_type::<StaleProposalError>(),
    )?;
    m.add("PolicyError", m.py().get_type::<PolicyError>())?;
    Ok(())
}

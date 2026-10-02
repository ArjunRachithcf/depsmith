//! PyO3 bindings behind the `depsmith` Python package (`depsmith._native`).
//! The typed Python API in `python/depsmith` wraps these functions; blocking
//! work releases the GIL and errors become typed exceptions.
use depsmith_core as core;
use pyo3::{create_exception, exceptions::PyRuntimeError, prelude::*};
use std::path::Path;

create_exception!(
    depsmith,
    ConfigurationError,
    PyRuntimeError,
    "Invalid options, configuration or targets, or an unmet precondition (CLI exit 2)."
);
create_exception!(
    depsmith,
    OperationError,
    PyRuntimeError,
    "A package manager, scanner or other operation failed (CLI exit 3)."
);
create_exception!(
    depsmith,
    StaleProposalError,
    PyRuntimeError,
    "The repository changed since the proposal was prepared; prepare it again."
);
create_exception!(
    depsmith,
    PolicyError,
    PyRuntimeError,
    "A configured policy, such as a vulnerability gate, rejected the proposal (CLI exit 4)."
);
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
        py.detach(|| core::apply(&self.inner, allow_partial))
            .map(|r| serde_json::to_string(&r).unwrap())
            .map_err(error)
    }
}
#[pyfunction]
fn discover_json(py: Python<'_>, root: String) -> PyResult<String> {
    py.detach(|| core::discover(Path::new(&root)))
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
    py.detach(|| {
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
    py.detach(|| {
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
    py.detach(|| {
        config(&root, &options).map(|settings| core::doctor(Path::new(&root), &settings.options))
    })
    .map(|r| serde_json::to_string(&r).unwrap())
    .map_err(error)
}
#[pyfunction]
fn init_json(py: Python<'_>, root: String, options: String, fetch_tools: bool) -> PyResult<String> {
    py.detach(|| {
        config(&root, &options).and_then(|settings| {
            core::init(Path::new(&root), &settings.options, &mut |_| fetch_tools)
        })
    })
    .map(|r| serde_json::to_string(&r).unwrap())
    .map_err(error)
}
#[pyfunction]
fn recover_json(py: Python<'_>, root: String) -> PyResult<String> {
    py.detach(|| core::recover(Path::new(&root)))
        .map(|r| serde_json::to_string(&r).unwrap())
        .map_err(error)
}
#[pyfunction]
fn cli(py: Python<'_>, args: Vec<String>) -> u8 {
    py.detach(|| depsmith_cli::run_from(args))
}
#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<NativeProposal>()?;
    m.add_function(wrap_pyfunction!(discover_json, m)?)?;
    m.add_function(wrap_pyfunction!(prepare, m)?)?;
    m.add_function(wrap_pyfunction!(scan_json, m)?)?;
    m.add_function(wrap_pyfunction!(doctor_json, m)?)?;
    m.add_function(wrap_pyfunction!(init_json, m)?)?;
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

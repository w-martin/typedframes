//! `typedframes._rust`: the command line, run inside the Python interpreter.

use pyo3::prelude::*;

/// Run the `typedframes` command line with `argv` (the program name first); returns the exit status.
#[pyfunction]
fn main(py: Python<'_>, argv: Vec<String>) -> u8 {
    // The checker uses its own threads and no Python objects, so the interpreter can run meanwhile.
    py.detach(|| typedframes_checker::cli::run(argv))
}

/// Like `main`, but returns the exit status, standard output and the failure message instead of writing them.
#[pyfunction]
fn run_captured(py: Python<'_>, argv: Vec<String>) -> (u8, String, String) {
    let (status, out, err) = py.detach(|| typedframes_checker::cli::run_captured(argv));
    (
        status,
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
    )
}

#[pymodule]
fn _rust(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(main, module)?)?;
    module.add_function(wrap_pyfunction!(run_captured, module)?)
}

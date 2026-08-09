//! The PyO3-facing entry points Python calls into.
//!
//! Module registration itself stays in `lib.rs`; this module holds the two
//! `#[pyfunction]`s and the process-wide deserialised-index cache.

use crate::config::{find_project_root, load_linter_config};
use crate::errors::{CheckFileResult, FileStats, LintError, CODE_UNTRACKED_DATAFRAME};
use crate::index::{build_index_internal, ProjectIndex};
use crate::notebook::{self, NotebookCheckResult, NotebookFileStats};
use crate::Linter;
use pyo3::prelude::*;
use ruff_notebook::{Notebook, NotebookError};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Check a single Python file for DataFrame column errors.
///
/// Accepts an optional MessagePack-serialised [`ProjectIndex`] (produced by
/// [`build_project_index`]) so the linter can resolve cross-file imports, e.g. a schema
/// defined in `schemas.py` and used in `pipeline.py`.  Returns a JSON array of
/// [`LintError`] objects, or `"[]"` when the linter is disabled in `pyproject.toml`.
#[pyfunction]
#[pyo3(signature = (file_path, index_bytes = None))]
pub(crate) fn check_file(file_path: String, index_bytes: Option<Vec<u8>>) -> PyResult<String> {
    let path = Path::new(&file_path);
    let project_root = find_project_root(path);
    let config = load_linter_config(&project_root);

    if !config.enabled.unwrap_or(true) {
        let empty = CheckFileResult {
            errors: Vec::new(),
            stats: FileStats::default(),
        };
        return serde_json::to_string(&empty)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("{}", e)));
    }

    let source = fs::read_to_string(path)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyIOError, _>(format!("{}", e)))?;

    let mut linter = Linter::new();
    linter.with_context(project_root.clone(), &config);

    // Diagnostics resolved at a call site in THIS file, targeting a function elsewhere
    // whose parameter governs a Feast features= call — see
    // resolve_param_governed_call_sites. Collected before check_file_internal runs so
    // they can be merged into its own diagnostics below. Kept across the whole
    // function (not just this block) since resolved_governed is also needed AFTER
    // check_file_internal runs, to decide which of THIS file's own intra-function
    // warnings are now stale.
    let mut call_site_errors: Vec<LintError> = Vec::new();
    let mut index: Option<Arc<ProjectIndex>> = None;
    if let Some(bytes) = index_bytes {
        if let Some(idx) = get_cached_index(&bytes) {
            linter.load_cross_file_symbols(&idx, &source, path, &project_root);
            if let Some(extra) = idx.call_site_errors.get(&file_path) {
                call_site_errors = extra.clone();
            }
            index = Some(idx);
        }
    }

    let mut errors = linter
        .check_file_internal(&source, path)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("{}", e)))?;

    // Retract THIS file's own intra-function untracked-dataframe warnings for any
    // function that at least one call site anywhere in the project was actually SEEN
    // for — see ProjectIndex.resolved_governed's doc comment for why this can't just
    // happen unconditionally whenever a function has the param-governed shape (a
    // function with no discoverable call site anywhere has nowhere else to put the
    // diagnostic, so it would be wrong to silently drop its only one). A seen call
    // site that failed to resolve gets its own untracked-dataframe diagnostic at the
    // call site instead — see check_governed_call_site.
    if let Some(index) = &index {
        for (func_name, template) in &linter.param_governed_templates {
            if index
                .resolved_governed
                .contains(&(file_path.clone(), func_name.clone()))
            {
                errors.retain(|e| {
                    !(e.line == template.governing_line
                        && e.col == template.governing_col
                        && e.code == CODE_UNTRACKED_DATAFRAME)
                });
            }
        }
    }

    errors.extend(call_site_errors);
    errors.sort_by_key(|e| (e.line, e.col));

    if !config.warnings.unwrap_or(true) {
        errors.retain(|e| e.severity != "warning");
    }

    let result = CheckFileResult {
        errors,
        stats: FileStats {
            dataframes_total: linter.dataframes_total,
            dataframes_typed: linter.dataframes_typed,
            untyped_sites: std::mem::take(&mut linter.untyped_sites),
        },
    };

    serde_json::to_string(&result)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("{}", e)))
}

/// Check a single Jupyter notebook (`.ipynb`) for DataFrame column errors.
///
/// Mirrors [`check_file`], but the source comes from `ruff_notebook::Notebook` --
/// the same crate Ruff and Pyrefly use for their own notebook linting -- instead of
/// reading the path directly: the notebook's code cells are parsed as JSON and
/// concatenated in memory, then checked exactly like a regular file's contents
/// (`Linter::check_notebook_internal` parses in Jupyter/IPython mode so magics don't
/// break the parse). Every diagnostic's line/col and any "(defined at ...)" message
/// reference are translated back from that concatenated source to the notebook
/// cell/line they actually came from before returning.
#[pyfunction]
#[pyo3(signature = (file_path, index_bytes = None))]
pub(crate) fn check_notebook(file_path: String, index_bytes: Option<Vec<u8>>) -> PyResult<String> {
    let path = Path::new(&file_path);
    let project_root = find_project_root(path);
    let config = load_linter_config(&project_root);

    if !config.enabled.unwrap_or(true) {
        let empty = NotebookCheckResult {
            errors: Vec::new(),
            stats: NotebookFileStats::default(),
        };
        return serde_json::to_string(&empty)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("{}", e)));
    }

    let notebook = Notebook::from_path(path).map_err(|e| match e {
        NotebookError::Io(io_err) => {
            PyErr::new::<pyo3::exceptions::PyOSError, _>(format!("{}", io_err))
        }
        other => PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("{}", other)),
    })?;
    if !notebook.is_python_notebook() {
        return Err(PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(
            "not a Python notebook (kernel is not Python)",
        ));
    }
    let source = notebook.source_code();
    let file_display = path.display().to_string();

    let mut linter = Linter::new();
    linter.with_context(project_root.clone(), &config);

    // See check_file's identical block for why call_site_errors/index are tracked
    // across the whole function. A notebook can never itself be the TARGET of a
    // cross-file call or import (Python can't `import` a `.ipynb`), so these will
    // always end up empty/no-op here -- kept for structural parity with check_file
    // rather than special-cased away, in case indexing ever covers notebooks too.
    let mut call_site_errors: Vec<LintError> = Vec::new();
    let mut index: Option<Arc<ProjectIndex>> = None;
    if let Some(bytes) = index_bytes {
        if let Some(idx) = get_cached_index(&bytes) {
            linter.load_cross_file_symbols_notebook(&idx, source, &project_root);
            if let Some(extra) = idx.call_site_errors.get(&file_path) {
                call_site_errors = extra.clone();
            }
            index = Some(idx);
        }
    }

    let mut errors = linter
        .check_notebook_internal(source, path)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("{}", e)))?;

    if let Some(index) = &index {
        for (func_name, template) in &linter.param_governed_templates {
            if index
                .resolved_governed
                .contains(&(file_path.clone(), func_name.clone()))
            {
                errors.retain(|e| {
                    !(e.line == template.governing_line
                        && e.col == template.governing_col
                        && e.code == CODE_UNTRACKED_DATAFRAME)
                });
            }
        }
    }

    errors.extend(call_site_errors);
    errors.sort_by_key(|e| (e.line, e.col));

    if !config.warnings.unwrap_or(true) {
        errors.retain(|e| e.severity != "warning");
    }

    let stats = FileStats {
        dataframes_total: linter.dataframes_total,
        dataframes_typed: linter.dataframes_typed,
        untyped_sites: std::mem::take(&mut linter.untyped_sites),
    };
    let result = notebook::translate_result(errors, stats, &file_display, notebook.index());

    serde_json::to_string(&result)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("{}", e)))
}

// The CLI calls check_file once per file in a project, passing the SAME serialised
// index_bytes on every call (see `_check_files` in cli.py, and mypy.py's per-file
// hook, which does the same across a single mypy run). Deserialising a project-wide
// index — which can be hundreds of KB for a large project, since it must carry every
// candidate delegate-target function, not just DataFrame-relevant ones — on every
// single file turns an O(1)-per-file operation into an O(project size)-per-file one,
// i.e. O(files^2) for a whole-project check. Cache the deserialised index keyed by a
// hash of its bytes (hashing is far cheaper than re-deserialising nested
// Strings/Vecs/HashMaps) so repeat calls with the same index are a cache hit; a
// different hash (e.g. a different project root between calls) correctly replaces
// the cached entry rather than silently reusing stale data.
pub(crate) static INDEX_CACHE: Mutex<Option<(u64, Arc<ProjectIndex>)>> = Mutex::new(None);

pub(crate) fn get_cached_index(bytes: &[u8]) -> Option<Arc<ProjectIndex>> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    let hash = hasher.finish();

    let mut cache = INDEX_CACHE.lock().ok()?;
    if let Some((cached_hash, index)) = cache.as_ref() {
        if *cached_hash == hash {
            return Some(index.clone());
        }
    }
    let index = Arc::new(rmp_serde::from_slice::<ProjectIndex>(bytes).ok()?);
    *cache = Some((hash, index.clone()));
    Some(index)
}

/// Build a cross-file symbol index for an entire project.
///
/// Walks all `.py` files under `project_root`, parses each one, and extracts
/// schema definitions and annotated function return types into a [`ProjectIndex`].
/// The index is serialised with MessagePack and returned as raw bytes so it can be
/// held in Python memory and passed to subsequent [`check_file`] calls without any
/// intermediate disk I/O.
#[pyfunction]
pub(crate) fn build_project_index(project_root: String) -> PyResult<Vec<u8>> {
    let root = Path::new(&project_root);
    let index = build_index_internal(root);
    rmp_serde::to_vec(&index)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("{}", e)))
}

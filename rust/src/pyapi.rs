//! The PyO3-facing entry points Python calls into.
//!
//! Module registration is in `lib.rs`; this module holds the `#[pyfunction]`s and the
//! process-wide deserialised-index cache.

use crate::apply::{apply_all, Selection};
use crate::config::{find_project_root, load_linter_config, LinterConfig};
use crate::errors::{
    CheckFileResult, FileStats, LintError, UntypedSiteUsage, CODE_UNTRACKED_DATAFRAME,
};
use crate::index::{build_index_cached, build_single_file_index_cached, ProjectIndex};
use crate::notebook::{self, NotebookCheckResult, NotebookFileStats};
use crate::parallel::parallel_map;
use crate::suggest::{Suggestion, Withheld};
use crate::Linter;
use pyo3::prelude::*;
use ruff_notebook::{Notebook, NotebookError};
use serde::Serialize;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::{Arc, Mutex};

// Joins `linter.untyped_sites` (which has var/line/col) with `linter.untyped_site_usage`
// (keyed by position only) into the flat, self-contained list `FileStats` exposes --
// must run before either map is taken/cleared, so it's a read-only borrow, not a take.
fn collect_untyped_site_usage(linter: &Linter) -> Vec<UntypedSiteUsage> {
    linter
        .untyped_sites
        .iter()
        .filter_map(|site| {
            linter
                .untyped_site_usage
                .get(&(site.line, site.col))
                .map(|usage| UntypedSiteUsage {
                    line: site.line,
                    col: site.col,
                    var: site.var.clone(),
                    accesses: usage.accesses.clone(),
                    other_uses: usage.other_uses.clone(),
                })
        })
        .collect()
}

// The linter for one file, with the project index's cross-file symbols loaded when
// there is one. Shared by every entry point so they agree on which sites are untyped,
// which can depend on the index.
pub(crate) fn linter_with_index(
    path: &Path,
    project_root: &Path,
    config: &LinterConfig,
    source: &str,
    index: Option<Arc<ProjectIndex>>,
) -> (Linter, Option<Arc<ProjectIndex>>) {
    let mut linter = Linter::new();
    linter.with_context(project_root.to_path_buf(), config);
    let index =
        index.inspect(|idx| linter.load_cross_file_symbols(idx, source, path, project_root));
    (linter, index)
}

#[derive(Serialize)]
struct SuggestFileResult {
    suggestions: Vec<Suggestion>,
    withheld: Vec<Withheld>,
}

/// Dry-run suggestions for one Python file: for each untyped DataFrame origin, either an
/// edit (as byte ranges plus a rendered diff) or the reason none could be made. Nothing
/// is written. Takes the same optional project index as [`check_file`] so it agrees
/// with `check` about which sites are untyped. `display_path` is only how the diff
/// headers name the file.
#[pyfunction]
#[pyo3(signature = (file_path, display_path, index_bytes = None))]
pub(crate) fn suggest_file(
    file_path: String,
    display_path: String,
    index_bytes: Option<&[u8]>,
) -> PyResult<String> {
    let path = Path::new(&file_path);
    let project_root = find_project_root(path);
    let config = load_linter_config(&project_root);

    let mut result = SuggestFileResult {
        suggestions: Vec::new(),
        withheld: Vec::new(),
    };
    if config.enabled.unwrap_or(true) {
        let source = fs::read_to_string(path)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyIOError, _>(format!("{}", e)))?;
        let (mut linter, _index) = linter_with_index(
            path,
            &project_root,
            &config,
            &source,
            index_bytes.and_then(get_cached_index),
        );
        linter.collect_suggestions = true;
        linter.suggest_display_path = display_path;
        linter
            .check_file_internal(&source, path)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("{}", e)))?;
        result.suggestions = std::mem::take(&mut linter.suggestions);
        result.withheld = std::mem::take(&mut linter.withheld);
    }

    serde_json::to_string(&result)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("{}", e)))
}

/// Bug-report material for the untyped site called `name` in one file, as JSON: its
/// masked verdict and a redacted snippet that is classified the same way (or why there
/// is none). `"null"` when the file has no site of that name.
#[pyfunction]
#[pyo3(signature = (file_path, name, index_bytes = None))]
pub(crate) fn report_file(
    file_path: String,
    name: String,
    index_bytes: Option<&[u8]>,
) -> PyResult<String> {
    let path = Path::new(&file_path);
    let project_root = find_project_root(path);
    let config = load_linter_config(&project_root);
    if !config.enabled.unwrap_or(true) {
        return Ok("null".to_string());
    }
    let source = fs::read_to_string(path)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyIOError, _>(format!("{}", e)))?;
    let (mut linter, _index) = linter_with_index(
        path,
        &project_root,
        &config,
        &source,
        index_bytes.and_then(get_cached_index),
    );
    linter.collect_suggestions = true;
    linter
        .check_file_internal(&source, path)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("{}", e)))?;
    serde_json::to_string(&crate::report::report_for(&source, &linter, &name))
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("{}", e)))
}

/// Check a single Python file for DataFrame column errors.
///
/// Accepts an optional MessagePack-serialised [`ProjectIndex`] (produced by
/// [`build_project_index`]) so the linter can resolve cross-file imports, e.g. a schema
/// defined in `schemas.py` and used in `pipeline.py`.  Returns a JSON array of
/// [`LintError`] objects, or `"[]"` when the linter is disabled in `pyproject.toml`.
#[pyfunction]
#[pyo3(signature = (file_path, index_bytes = None))]
pub(crate) fn check_file(file_path: String, index_bytes: Option<&[u8]>) -> PyResult<String> {
    check_file_impl(&file_path, index_bytes.and_then(get_cached_index)).map_err(PyErr::from)
}

/// Why checking one file produced no result. Kept free of Python types so it can be
/// produced on a worker thread; [`check_file`]/[`check_notebook`] map it to `OSError` /
/// `RuntimeError`.
pub(crate) enum CheckError {
    /// The file couldn't be read.
    Io(String),
    /// The file isn't something the checker can parse (syntax error, bad notebook).
    Runtime(String),
}

impl CheckError {
    fn message(&self) -> &str {
        match self {
            CheckError::Io(m) | CheckError::Runtime(m) => m,
        }
    }
}

impl From<CheckError> for PyErr {
    fn from(error: CheckError) -> PyErr {
        match error {
            CheckError::Io(m) => PyErr::new::<pyo3::exceptions::PyOSError, _>(m),
            CheckError::Runtime(m) => PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(m),
        }
    }
}

fn runtime(e: impl std::fmt::Display) -> CheckError {
    CheckError::Runtime(e.to_string())
}

/// Why files under `root` shouldn't be edited (not a git work tree, or it has uncommitted
/// changes), or `None` when it is safe to.
#[pyfunction]
pub(crate) fn vcs_problem(root: String) -> Option<String> {
    crate::apply::vcs_problem(Path::new(&root))
}

/// `(file_path, display_path, origins)`: the origins to apply, or `None` for the safe tier.
type ApplyRequest = (String, String, Option<Vec<(usize, usize)>>);

/// Apply suggestions to files. `requests` is a list of `(file_path, display_path, origins)`
/// where `origins` is `None` for "every safe suggestion" or a list of `(line, col)` origin
/// positions the user chose. Each file's edits are checked in memory first and only
/// accepted if they add no errors; accepted files are then written together or not at all.
/// Returns JSON describing what happened to every file.
#[pyfunction]
#[pyo3(signature = (requests, index_bytes = None, require_tracked = false))]
pub(crate) fn apply_suggestions(
    py: Python<'_>,
    requests: Vec<ApplyRequest>,
    index_bytes: Option<&[u8]>,
    require_tracked: bool,
) -> PyResult<String> {
    let index = index_bytes.and_then(get_cached_index);
    let requests: Vec<(String, String, Selection)> = requests
        .into_iter()
        .map(|(file, display, origins)| {
            let selection = origins.map_or(Selection::SafeTier, Selection::Origins);
            (file, display, selection)
        })
        .collect();
    let result = py.detach(|| apply_all(&requests, index, require_tracked));
    serde_json::to_string(&result)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string()))
}

/// Check many files at once, in parallel, returning for each `(result_json, error)`
/// exactly one of which is set: the same JSON [`check_file`]/[`check_notebook`] return
/// (chosen by the `.ipynb` extension), or the message of the error they would raise.
/// Results are in input order. The GIL is released while the files are checked.
#[pyfunction]
#[pyo3(signature = (file_paths, index_bytes = None))]
pub(crate) fn check_files(
    py: Python<'_>,
    file_paths: Vec<String>,
    index_bytes: Option<&[u8]>,
) -> Vec<(Option<String>, Option<String>)> {
    let index = index_bytes.and_then(get_cached_index);
    py.detach(|| {
        parallel_map(&file_paths, |file_path| {
            let result = if file_path.ends_with(".ipynb") {
                check_notebook_impl(file_path, index.clone())
            } else {
                check_file_impl(file_path, index.clone())
            };
            match result {
                Ok(json) => (Some(json), None),
                Err(error) => (None, Some(error.message().to_string())),
            }
        })
    })
}

fn check_file_impl(
    file_path: &str,
    index: Option<Arc<ProjectIndex>>,
) -> Result<String, CheckError> {
    let file_path = file_path.to_string();
    let path = Path::new(&file_path);
    let project_root = find_project_root(path);
    let config = load_linter_config(&project_root);

    if !config.enabled.unwrap_or(true) {
        let empty = CheckFileResult {
            errors: Vec::new(),
            stats: FileStats::default(),
        };
        return serde_json::to_string(&empty).map_err(runtime);
    }

    let source = fs::read_to_string(path).map_err(|e| CheckError::Io(e.to_string()))?;

    let (mut linter, index) = linter_with_index(path, &project_root, &config, &source, index);

    // Diagnostics resolved at a call site in THIS file, targeting a function elsewhere
    // whose parameter governs a Feast features= call — see
    // resolve_param_governed_call_sites. Collected before check_file_internal runs so
    // they can be merged into its own diagnostics below. Kept across the whole
    // function (not just this block) since resolved_governed is also needed AFTER
    // check_file_internal runs, to decide which of THIS file's own intra-function
    // warnings are now stale.
    let call_site_errors: Vec<LintError> = index
        .as_ref()
        .and_then(|idx| idx.call_site_errors.get(&file_path).cloned())
        .unwrap_or_default();

    let mut errors = linter.check_file_internal(&source, path).map_err(runtime)?;

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

    let untyped_site_usage = collect_untyped_site_usage(&linter);
    let result = CheckFileResult {
        errors,
        stats: FileStats {
            dataframes_total: linter.dataframes_total,
            dataframes_typed: linter.dataframes_typed,
            untyped_sites: std::mem::take(&mut linter.untyped_sites),
            typed_sites: std::mem::take(&mut linter.typed_sites),
            all_dataframe_calls: std::mem::take(&mut linter.all_dataframe_calls),
            leg_events: std::mem::take(&mut linter.leg_events),
            untyped_site_usage,
        },
    };

    serde_json::to_string(&result).map_err(runtime)
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
pub(crate) fn check_notebook(file_path: String, index_bytes: Option<&[u8]>) -> PyResult<String> {
    check_notebook_impl(&file_path, index_bytes.and_then(get_cached_index)).map_err(PyErr::from)
}

fn check_notebook_impl(
    file_path: &str,
    index: Option<Arc<ProjectIndex>>,
) -> Result<String, CheckError> {
    let file_path = file_path.to_string();
    let path = Path::new(&file_path);
    let project_root = find_project_root(path);
    let config = load_linter_config(&project_root);

    if !config.enabled.unwrap_or(true) {
        let empty = NotebookCheckResult {
            errors: Vec::new(),
            stats: NotebookFileStats::default(),
        };
        return serde_json::to_string(&empty).map_err(runtime);
    }

    let notebook = Notebook::from_path(path).map_err(|e| match e {
        NotebookError::Io(io_err) => CheckError::Io(io_err.to_string()),
        other => runtime(other),
    })?;
    if !notebook.is_python_notebook() {
        return Err(runtime("not a Python notebook (kernel is not Python)"));
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
    if let Some(idx) = &index {
        linter.load_cross_file_symbols_notebook(idx, source, &project_root);
        if let Some(extra) = idx.call_site_errors.get(&file_path) {
            call_site_errors = extra.clone();
        }
    }

    let mut errors = linter
        .check_notebook_internal(source, path)
        .map_err(runtime)?;

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

    let untyped_site_usage = collect_untyped_site_usage(&linter);
    let stats = FileStats {
        dataframes_total: linter.dataframes_total,
        dataframes_typed: linter.dataframes_typed,
        untyped_sites: std::mem::take(&mut linter.untyped_sites),
        typed_sites: std::mem::take(&mut linter.typed_sites),
        all_dataframe_calls: std::mem::take(&mut linter.all_dataframe_calls),
        leg_events: std::mem::take(&mut linter.leg_events),
        untyped_site_usage,
    };
    let result = notebook::translate_result(errors, stats, &file_display, notebook.index());

    serde_json::to_string(&result).map_err(runtime)
}

// Every file in a run is checked against the same serialised index. Deserialising it per
// file would make a whole-project check O(files^2), so the last one is kept, keyed by the
// id stamped on its bytes (see `INDEX_ID_LEN`); a different index replaces the entry.
pub(crate) static INDEX_CACHE: Mutex<Option<(u128, Arc<ProjectIndex>)>> = Mutex::new(None);

// A unique id leads each serialized index, so telling indexes apart is a 16-byte compare
// rather than a pass over a payload that can be tens of MB.
const INDEX_ID_LEN: usize = 16;

fn new_index_id() -> u128 {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (
        nanos,
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
    )
        .hash(&mut hasher);
    (u128::from(hasher.finish()) << 64) | (nanos & u128::from(u64::MAX))
}

pub(crate) fn encode_index(index: &ProjectIndex) -> Result<Vec<u8>, rmp_serde::encode::Error> {
    let mut bytes = new_index_id().to_le_bytes().to_vec();
    index.serialize(&mut rmp_serde::Serializer::new(&mut bytes))?;
    Ok(bytes)
}

pub(crate) fn get_cached_index(bytes: &[u8]) -> Option<Arc<ProjectIndex>> {
    let (id, body) = bytes.split_at_checked(INDEX_ID_LEN)?;
    let id = u128::from_le_bytes(id.try_into().ok()?);

    let mut cache = INDEX_CACHE.lock().ok()?;
    if let Some((cached_id, index)) = cache.as_ref() {
        if *cached_id == id {
            return Some(index.clone());
        }
    }
    let index = Arc::new(rmp_serde::from_slice::<ProjectIndex>(body).ok()?);
    *cache = Some((id, index.clone()));
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
#[pyo3(signature = (project_root, cache_dir = None))]
pub(crate) fn build_project_index(
    py: Python<'_>,
    project_root: String,
    cache_dir: Option<String>,
) -> PyResult<Vec<u8>> {
    let root = Path::new(&project_root);
    let index = py.detach(|| build_index_cached(root, cache_dir.as_deref().map(Path::new)));
    encode_index(&index)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("{}", e)))
}

/// Build a cross-file symbol index scoped to a SINGLE file's own references.
///
/// The counterpart to [`build_project_index`] for `typedframes check some_file.py`.
/// Rather than walking the whole project, it locates the project root by walking up
/// from `file_path` to the nearest `pyproject.toml` and indexes only that file, the
/// project-local modules its own imports reach (bounded — see
/// `MAX_SINGLE_FILE_IMPORT_DEPTH`), and any external installed package that closure
/// calls in a DataFrame-shaped way. Same MessagePack-serialised [`ProjectIndex`] shape,
/// so [`check_file`] consumes it identically.
///
/// Returns `None` when no `pyproject.toml` exists anywhere above the file — there is
/// then no root to resolve imports against, so the caller checks the file in isolation
/// exactly as it did before.
#[pyfunction]
#[pyo3(signature = (file_path, cache_dir = None))]
pub(crate) fn build_single_file_index(
    py: Python<'_>,
    file_path: String,
    cache_dir: Option<String>,
) -> PyResult<Option<Vec<u8>>> {
    let path = Path::new(&file_path);
    let Some(index) =
        py.detach(|| build_single_file_index_cached(path, cache_dir.as_deref().map(Path::new)))
    else {
        return Ok(None);
    };
    encode_index(&index)
        .map(Some)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("{}", e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_index() -> ProjectIndex {
        let dir = tempfile::tempdir().unwrap();
        build_index_cached(dir.path(), None)
    }

    #[test]
    fn test_should_stamp_each_encoded_index_with_a_distinct_id() {
        let index = empty_index();

        let first = encode_index(&index).unwrap();
        let second = encode_index(&index).unwrap();

        assert_ne!(first[..INDEX_ID_LEN], second[..INDEX_ID_LEN]);
        assert_eq!(first[INDEX_ID_LEN..], second[INDEX_ID_LEN..]);
    }

    #[test]
    fn test_should_decode_what_it_encoded() {
        let bytes = encode_index(&empty_index()).unwrap();

        assert!(get_cached_index(&bytes).is_some());
    }

    #[test]
    fn test_should_reject_bytes_too_short_to_hold_an_id() {
        assert!(get_cached_index(&[1, 2, 3]).is_none());
        assert!(get_cached_index(&[]).is_none());
    }

    #[test]
    fn test_should_reject_a_payload_that_is_not_an_index() {
        // a fresh id, so the cache can't answer for it
        let mut bytes = new_index_id().to_le_bytes().to_vec();
        bytes.extend_from_slice(b"not messagepack");

        assert!(get_cached_index(&bytes).is_none());
    }
}

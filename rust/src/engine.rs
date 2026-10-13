//! The Python-free core the extension and the command line both call: per-file checks,
//! suggestions and reports, the project contexts they share, and the process-wide
//! deserialised-index cache.

use crate::config::{find_project_root, find_project_root_opt, load_linter_config, LinterConfig};
use crate::errors::{
    CheckFileResult, FileStats, LintError, UntypedSiteUsage, CODE_UNTRACKED_DATAFRAME,
};
use crate::index::ProjectIndex;
use crate::notebook::{self, NotebookCheckResult, NotebookFileStats};
use crate::parallel::{file_cost, parallel_map_largest_first};
use crate::suggest::{Suggestion, Withheld};
use crate::Linter;
use ruff_notebook::{Notebook, NotebookError};
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

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
    let index = index.inspect(|idx| {
        linter.load_cross_file_symbols(idx, source, path, project_root);
        linter.project_index = Some(Arc::clone(idx));
    });
    (linter, index)
}

#[derive(Serialize)]
struct SuggestFileResult {
    suggestions: Vec<Suggestion>,
    withheld: Vec<Withheld>,
}

pub(crate) fn suggest_file_impl(
    file_path: &str,
    display_path: &str,
    index: Option<Arc<ProjectIndex>>,
    contexts: &ProjectContexts,
) -> Result<String, CheckError> {
    let path = Path::new(file_path);
    let context = contexts.of(path);
    let (project_root, config) = (&context.root, &context.config);

    let mut result = SuggestFileResult {
        suggestions: Vec::new(),
        withheld: Vec::new(),
    };
    if config.enabled.unwrap_or(true) {
        let source = fs::read_to_string(path).map_err(|e| CheckError::Io(e.to_string()))?;
        let (mut linter, _index) = linter_with_index(path, project_root, config, &source, index);
        linter.collect_suggestions = true;
        linter.suggest_display_path = display_path.to_string();
        linter.check_file_internal(&source, path).map_err(runtime)?;
        result.suggestions = std::mem::take(&mut linter.suggestions);
        result.withheld = std::mem::take(&mut linter.withheld);
    }

    serde_json::to_string(&result).map_err(runtime)
}

// Runs `f` over `items` in parallel, largest file first, as `(json, error)` pairs in input
// order. `path_of` names the file an item is about, for ordering by size.
pub(crate) fn run_batch<T: Sync>(
    items: &[T],
    path_of: impl Fn(&T) -> &String,
    f: impl Fn(&T) -> Result<String, CheckError> + Sync,
) -> Vec<(Option<String>, Option<String>)> {
    parallel_map_largest_first(
        items,
        |item| file_cost(Path::new(path_of(item))),
        |item| match f(item) {
            Ok(json) => (Some(json), None),
            Err(error) => (None, Some(error.message().to_string())),
        },
    )
}

// Checks `file_path` as `report` and `report_all` need it: with suggestions collected.
// `None` when the linter is disabled in the project's `pyproject.toml`.
pub(crate) fn checked_for_report(
    file_path: &str,
    index: Option<Arc<ProjectIndex>>,
    contexts: &ProjectContexts,
) -> Result<Option<(String, Linter)>, CheckError> {
    let path = Path::new(file_path);
    let context = contexts.of(path);
    let (project_root, config) = (&context.root, &context.config);
    if !config.enabled.unwrap_or(true) {
        return Ok(None);
    }
    let source = fs::read_to_string(path).map_err(|e| CheckError::Io(e.to_string()))?;
    let (mut linter, _index) = linter_with_index(path, project_root, config, &source, index);
    linter.collect_suggestions = true;
    linter.check_file_internal(&source, path).map_err(runtime)?;
    Ok(Some((source, linter)))
}

pub(crate) fn report_file_impl(
    file_path: &str,
    name: &str,
    index: Option<Arc<ProjectIndex>>,
    contexts: &ProjectContexts,
) -> Result<String, CheckError> {
    let report = checked_for_report(file_path, index, contexts)?
        .and_then(|(source, linter)| crate::report::report_for(&source, &linter, name));
    serde_json::to_string(&report).map_err(runtime)
}

pub(crate) fn report_all_file_impl(
    file_path: &str,
    index: Option<Arc<ProjectIndex>>,
    contexts: &ProjectContexts,
) -> Result<String, CheckError> {
    let reports = checked_for_report(file_path, index, contexts)?
        .map(|(source, linter)| crate::report::report_all(&source, &linter))
        .unwrap_or_default();
    serde_json::to_string(&reports).map_err(runtime)
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
    pub(crate) fn message(&self) -> &str {
        match self {
            CheckError::Io(m) | CheckError::Runtime(m) => m,
        }
    }
}

pub(crate) fn runtime(e: impl std::fmt::Display) -> CheckError {
    CheckError::Runtime(e.to_string())
}

/// A file's check result as JSON, or the message of why it was skipped: exactly one is set.
pub(crate) type FileOutcome = (Option<String>, Option<String>);

/// Checks `files` in parallel, largest first, as one outcome per file in input order. A path
/// ending in `.ipynb` is checked as a notebook.
pub(crate) fn check_batch(files: &[String], index: Option<Arc<ProjectIndex>>) -> Vec<FileOutcome> {
    let contexts = ProjectContexts::default();
    parallel_map_largest_first(
        files,
        |file_path| file_cost(Path::new(file_path)),
        |file_path| {
            let result = if file_path.ends_with(".ipynb") {
                check_notebook_impl(file_path, index.clone())
            } else {
                check_file_impl(file_path, index.clone(), &contexts)
            };
            match result {
                Ok(json) => (Some(json), None),
                Err(error) => (None, Some(error.message().to_string())),
            }
        },
    )
}

/// Dry-run suggestions for each `(file, display name)` request, in parallel, as one outcome per
/// request in input order. The display name only names the file in diff headers.
pub(crate) fn suggest_batch(
    requests: &[(String, String)],
    index: Option<Arc<ProjectIndex>>,
) -> Vec<FileOutcome> {
    let contexts = ProjectContexts::default();
    run_batch(
        requests,
        |(file, _)| file,
        |(file, display)| suggest_file_impl(file, display, index.clone(), &contexts),
    )
}

/// The report for the untyped site called `name` in each file (JSON `null` where a file has none),
/// in parallel, as one outcome per file in input order.
pub(crate) fn report_batch(
    files: &[String],
    name: &str,
    index: Option<Arc<ProjectIndex>>,
) -> Vec<FileOutcome> {
    let contexts = ProjectContexts::default();
    run_batch(
        files,
        |file| file,
        |file| report_file_impl(file, name, index.clone(), &contexts),
    )
}

/// Every untyped site's report in each file (a JSON array), in parallel, as one outcome per file in
/// input order.
pub(crate) fn report_all_batch(
    files: &[String],
    index: Option<Arc<ProjectIndex>>,
) -> Vec<FileOutcome> {
    let contexts = ProjectContexts::default();
    run_batch(
        files,
        |file| file,
        |file| report_all_file_impl(file, index.clone(), &contexts),
    )
}

// A project's root and `[tool.typedframes]` settings.
pub(crate) struct ProjectContext {
    root: PathBuf,
    config: LinterConfig,
}

// The root and settings of the project each file belongs to, found once per directory
// rather than once per file: a batch of files shares a handful of directories, and reading
// and parsing `pyproject.toml` for every one of them is pure repetition. A directory with no
// `pyproject.toml` above it has no shared root -- each file there is its own root, as
// `find_project_root` falls back to -- so only the fact that none exists is remembered.
#[derive(Default)]
pub(crate) struct ProjectContexts {
    by_dir: RwLock<HashMap<PathBuf, Option<Arc<ProjectContext>>>>,
    by_root: RwLock<HashMap<PathBuf, Arc<ProjectContext>>>,
}

impl ProjectContexts {
    fn of(&self, path: &Path) -> Arc<ProjectContext> {
        let dir = path.parent().unwrap_or(path);
        let cached = self.by_dir.read().ok().and_then(|m| m.get(dir).cloned());
        let found = cached.unwrap_or_else(|| {
            let found = find_project_root_opt(path).map(|root| self.shared(root));
            if let Ok(mut dirs) = self.by_dir.write() {
                dirs.insert(dir.to_path_buf(), found.clone());
            }
            found
        });
        // A file is its own root here, and a file holds no `pyproject.toml`.
        found.unwrap_or_else(|| {
            Arc::new(ProjectContext {
                root: path.to_path_buf(),
                config: LinterConfig::EMPTY,
            })
        })
    }

    fn shared(&self, root: PathBuf) -> Arc<ProjectContext> {
        if let Some(known) = self.by_root.read().ok().and_then(|m| m.get(&root).cloned()) {
            return known;
        }
        let context = Arc::new(ProjectContext {
            config: load_linter_config(&root),
            root: root.clone(),
        });
        if let Ok(mut roots) = self.by_root.write() {
            roots.insert(root, Arc::clone(&context));
        }
        context
    }
}

pub(crate) fn check_file_impl(
    file_path: &str,
    index: Option<Arc<ProjectIndex>>,
    contexts: &ProjectContexts,
) -> Result<String, CheckError> {
    let file_path = file_path.to_string();
    let path = Path::new(&file_path);
    let context = contexts.of(path);
    let (project_root, config) = (&context.root, &context.config);

    if !config.enabled.unwrap_or(true) {
        let empty = CheckFileResult {
            errors: Vec::new(),
            stats: FileStats::default(),
        };
        return serde_json::to_string(&empty).map_err(runtime);
    }

    let source = fs::read_to_string(path).map_err(|e| CheckError::Io(e.to_string()))?;

    // Parsed once, for the cross-file symbol load and for the lint itself.
    let parsed = ruff_python_parser::parse_module(&source).ok();
    let module = parsed.as_ref().map(|parsed| parsed.syntax());
    let mut linter = Linter::new();
    linter.with_context(project_root.clone(), config);
    let index = index.inspect(|idx| {
        if let Some(module) = module {
            linter.load_cross_file_symbols_from_module(idx, module, project_root, path.to_str());
        }
        linter.project_index = Some(Arc::clone(idx));
    });

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

    let mut errors = match parsed {
        Some(parsed) => linter.check_parsed_module(parsed.into_syntax(), &source, path),
        None => linter.check_file_internal(&source, path).map_err(runtime)?,
    };

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

pub(crate) fn check_notebook_impl(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_give_each_file_its_own_root_when_no_project_file_is_above_it() {
        // arrange: with no `pyproject.toml` the file's own path is its root, so one file's
        // answer must never be handed to its neighbours.
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.py"), dir.path().join("b.py"));
        fs::write(&a, "x = 1\n").unwrap();
        fs::write(&b, "y = 2\n").unwrap();
        let contexts = ProjectContexts::default();

        // act
        let (first, second) = (contexts.of(&a), contexts.of(&b));

        // assert
        assert_eq!(first.root, a);
        assert_eq!(second.root, b);
    }

    #[test]
    fn test_should_share_one_context_between_files_of_the_same_project() {
        // arrange
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("pyproject.toml"), "").unwrap();
        let (a, b) = (dir.path().join("a.py"), dir.path().join("b.py"));
        fs::write(&a, "x = 1\n").unwrap();
        fs::write(&b, "y = 2\n").unwrap();
        let contexts = ProjectContexts::default();

        // act
        let (first, second) = (contexts.of(&a), contexts.of(&b));

        // assert
        assert_eq!(first.root, dir.path());
        assert!(Arc::ptr_eq(&first, &second));
    }
}

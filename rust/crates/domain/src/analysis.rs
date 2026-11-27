//! The analysis of one file, as the application layer asks for it: the file's text (or parsed
//! notebook), where it sits in its project, and the project index. Nothing here reads a file.

use crate::config::LinterConfig;
use crate::errors::{
    CheckFileResult, FileStats, LintError, UntypedSiteUsage, CODE_UNTRACKED_DATAFRAME,
};
use crate::index::ProjectIndex;
use crate::notebook::{self, NotebookCheckResult};
use crate::ports::FileSystem;
use crate::report::SiteReport;
use crate::suggest::{Suggestion, Withheld};
use crate::Linter;
use ruff_notebook::Notebook;
use serde::Serialize;
use std::path::Path;
use std::sync::Arc;

/// Where a file sits: its path, its project's root and settings, and the project index.
#[derive(Clone)]
pub struct FileContext<'a> {
    /// Where imported modules and `.sql` files are read from.
    pub fs: Arc<dyn FileSystem>,
    pub path: &'a Path,
    pub project_root: &'a Path,
    pub config: &'a LinterConfig,
    pub index: Option<Arc<ProjectIndex>>,
}

/// What `suggest` found for one file.
#[derive(Serialize, Default)]
pub struct Suggestions {
    pub suggestions: Vec<Suggestion>,
    pub withheld: Vec<Withheld>,
}

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

fn linter_for(context: &FileContext) -> Linter {
    let mut linter = Linter::new();
    linter.fs = Arc::clone(&context.fs).into();
    linter
}

// The linter for one file, with the project index's cross-file symbols loaded when
// there is one. Shared by every entry point so they agree on which sites are untyped,
// which can depend on the index.
pub(crate) fn linter_with_index(context: &FileContext, source: &str) -> Linter {
    let mut linter = linter_for(context);
    linter.with_context(context.project_root.to_path_buf(), context.config);
    if let Some(idx) = &context.index {
        linter.load_cross_file_symbols(idx, source, context.path, context.project_root);
        linter.project_index = Some(Arc::clone(idx));
    }
    linter
}

/// `linter_with_index` for a notebook's concatenated source.
pub(crate) fn notebook_linter(context: &FileContext, source: &str) -> Linter {
    let mut linter = linter_for(context);
    linter.with_context(context.project_root.to_path_buf(), context.config);
    if let Some(idx) = &context.index {
        linter.load_cross_file_symbols_notebook(idx, source, context.project_root);
    }
    linter
}

// The project-wide corrections to a file's errors: a governed template the index resolved
// is not untracked, call-site errors the index found are added, warnings go if the project
// turned them off; sorted by position.
fn settle_errors(
    context: &FileContext,
    linter: &Linter,
    mut errors: Vec<LintError>,
    call_site_errors: Vec<LintError>,
) -> Vec<LintError> {
    let file_path = context.path.to_string_lossy().into_owned();
    if let Some(index) = &context.index {
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
    if !context.config.warnings.unwrap_or(true) {
        errors.retain(|e| e.severity != "warning");
    }
    errors
}

fn stats_of(linter: &mut Linter) -> FileStats {
    let untyped_site_usage = collect_untyped_site_usage(linter);
    FileStats {
        dataframes_total: linter.dataframes_total,
        dataframes_typed: linter.dataframes_typed,
        dataframes_unresolved: linter.dataframes_unresolved,
        untyped_sites: std::mem::take(&mut linter.untyped_sites),
        typed_sites: std::mem::take(&mut linter.typed_sites),
        all_dataframe_calls: std::mem::take(&mut linter.all_dataframe_calls),
        leg_events: std::mem::take(&mut linter.leg_events),
        untyped_site_usage,
    }
}

fn call_site_errors(context: &FileContext) -> Vec<LintError> {
    let file_path = context.path.to_string_lossy();
    context
        .index
        .as_ref()
        .and_then(|idx| idx.call_site_errors.get(file_path.as_ref()).cloned())
        .unwrap_or_default()
}

/// Checks a Python file's `source`. An error is why it could not be checked (it does not parse).
pub fn check_python(context: &FileContext, source: &str) -> Result<CheckFileResult, String> {
    let path = context.path;
    let parsed = ruff_python_parser::parse_module(source).ok();
    let module = parsed.as_ref().map(|parsed| parsed.syntax());
    let mut linter = linter_for(context);
    linter.with_context(context.project_root.to_path_buf(), context.config);
    if let Some(idx) = &context.index {
        if let Some(module) = module {
            linter.load_cross_file_symbols_from_module(
                idx,
                module,
                context.project_root,
                path.to_str(),
            );
        }
        linter.project_index = Some(Arc::clone(idx));
    }
    let call_site_errors = call_site_errors(context);
    let errors = match parsed {
        Some(parsed) => linter.check_parsed_module(parsed.into_syntax(), source, path),
        None => linter
            .check_file_internal(source, path)
            .map_err(|e| e.to_string())?,
    };
    let errors = settle_errors(context, &linter, errors, call_site_errors);
    Ok(CheckFileResult {
        errors,
        stats: stats_of(&mut linter),
    })
}

/// Checks a parsed Python notebook, with positions translated to cells.
pub fn check_notebook(
    context: &FileContext,
    notebook: &Notebook,
) -> Result<NotebookCheckResult, String> {
    let source = notebook.source_code();
    let mut linter = notebook_linter(context, source);
    let call_site_errors = call_site_errors(context);
    let errors = linter
        .check_notebook_internal(source, context.path)
        .map_err(|e| e.to_string())?;
    let errors = settle_errors(context, &linter, errors, call_site_errors);
    let stats = stats_of(&mut linter);
    let file_display = context.path.display().to_string();
    Ok(notebook::translate_result(
        errors,
        stats,
        &file_display,
        notebook.index(),
    ))
}

/// The suggestions for a Python file's `source`; `display_path` names it in diff headers.
pub fn suggest_python(
    context: &FileContext,
    display_path: &str,
    source: &str,
) -> Result<Suggestions, String> {
    let mut linter = linter_with_index(context, source);
    linter.collect_suggestions = true;
    linter.suggest_display_path = display_path.to_string();
    linter
        .check_file_internal(source, context.path)
        .map_err(|e| e.to_string())?;
    Ok(Suggestions {
        suggestions: std::mem::take(&mut linter.suggestions),
        withheld: std::mem::take(&mut linter.withheld),
    })
}

/// The suggestions for a parsed notebook, each with the cell it is in.
pub fn suggest_notebook(
    context: &FileContext,
    display_path: &str,
    notebook: &Notebook,
) -> Result<Suggestions, String> {
    let source = notebook.source_code();
    let mut linter = notebook_linter(context, source);
    linter.collect_suggestions = true;
    linter.suggest_display_path = display_path.to_string();
    linter
        .check_notebook_internal(source, context.path)
        .map_err(|e| e.to_string())?;
    let mut result = Suggestions {
        suggestions: std::mem::take(&mut linter.suggestions),
        withheld: std::mem::take(&mut linter.withheld),
    };
    let cell_of = |line| Some(notebook::translate(notebook.index(), line));
    for suggestion in &mut result.suggestions {
        suggestion.cell = cell_of(suggestion.line);
    }
    for withheld in &mut result.withheld {
        withheld.cell = cell_of(withheld.line);
    }
    Ok(result)
}

// Checks `source` as `report` needs it: with suggestions collected.
fn checked_for_report(context: &FileContext, source: &str) -> Result<Linter, String> {
    let mut linter = linter_with_index(context, source);
    linter.collect_suggestions = true;
    linter
        .check_file_internal(source, context.path)
        .map_err(|e| e.to_string())?;
    Ok(linter)
}

/// The report for the untyped site called `name` in `source`, if it has one.
pub fn report_site(
    context: &FileContext,
    source: &str,
    name: &str,
) -> Result<Option<SiteReport>, String> {
    let linter = checked_for_report(context, source)?;
    Ok(crate::report::report_for(source, &linter, name))
}

/// Every untyped site's report in `source`, or with `untracked` the report of every frame the
/// checker does not track.
pub fn report_sites(
    context: &FileContext,
    source: &str,
    untracked: bool,
) -> Result<Vec<SiteReport>, String> {
    let linter = checked_for_report(context, source)?;
    Ok(if untracked {
        crate::report::untracked_all(source, &linter)
    } else {
        crate::report::report_all(source, &linter)
    })
}

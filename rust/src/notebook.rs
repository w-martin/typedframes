//! Jupyter notebook (.ipynb) support built on `ruff_notebook` -- the same crate
//! Ruff and Pyrefly use for their own notebook linting.
//!
//! `ruff_notebook::Notebook` loads the `.ipynb` JSON and exposes a concatenated
//! Python source (`source_code()`) plus a `NotebookIndex` mapping concatenated-source
//! lines back to `(cell, cell-relative line)`. The linter (see
//! `Linter::check_notebook_internal` in `linter.rs`) runs against that concatenated
//! source exactly as it would against a real `.py` file's contents -- this module's
//! only job is translating its output back to notebook coordinates afterwards.

use ruff_notebook::NotebookIndex;
use ruff_source_file::OneIndexed;
use serde::Serialize;

use crate::errors::{FileStats, LintError};

/// One diagnostic, relocated from the synthetic concatenated-source line/col
/// `Linter::check_notebook_internal` actually parsed to the notebook cell/line
/// it came from. `cell` is 1-indexed among ALL cells in the notebook (markdown
/// and raw included), matching `ruff_notebook::NotebookIndex`'s own convention
/// -- not "the Nth code cell".
#[derive(Debug, Serialize)]
pub(crate) struct NotebookLintError {
    pub cell: usize,
    pub line: usize,
    pub col: usize,
    pub code: String,
    pub message: String,
    pub severity: String,
}

/// The untyped-DataFrame-site counterpart of [`NotebookLintError`].
#[derive(Debug, Serialize)]
pub(crate) struct NotebookUntypedSite {
    pub cell: usize,
    pub line: usize,
    pub col: usize,
    pub var: String,
}

/// The notebook counterpart of [`FileStats`], nested under `stats` in
/// [`NotebookCheckResult`] -- matching [`crate::errors::CheckFileResult`]'s shape so
/// the Python side can treat both JSON payloads the same way.
#[derive(Debug, Default, Serialize)]
pub(crate) struct NotebookFileStats {
    pub dataframes_total: usize,
    pub dataframes_typed: usize,
    pub untyped_sites: Vec<NotebookUntypedSite>,
}

/// The JSON payload returned by the `check_notebook` entry point. Same shape as
/// [`crate::errors::CheckFileResult`], the `check_file` counterpart.
#[derive(Debug, Serialize)]
pub(crate) struct NotebookCheckResult {
    pub errors: Vec<NotebookLintError>,
    pub stats: NotebookFileStats,
}

/// Translate a concatenated-source line (1-indexed) to `(cell, cell-relative line)`,
/// both 1-indexed. Falls back to `(1, line)` for a line the index has no cell for --
/// defensive only: every line the linter reports comes from the exact source the
/// index was itself built from, so a miss here should never actually happen.
fn translate(index: &NotebookIndex, line: usize) -> (usize, usize) {
    let Some(row) = OneIndexed::new(line) else {
        return (1, line);
    };
    let cell = index.cell(row).unwrap_or(OneIndexed::MIN);
    let cell_row = index.cell_row(row).unwrap_or(row);
    (cell.get(), cell_row.get())
}

/// Rewrite `<file_display>:<concatenated line>` references inside a message to
/// `<file_display> cell <N>:<cell line>`.
///
/// Covers a schema defined earlier in the SAME notebook, whose "(defined at ...)"
/// cross-reference (see `Linter::schema_display`) would otherwise cite a line number
/// from the synthetic concatenated source -- meaningless in the actual `.ipynb` file,
/// which is JSON, not source text.
fn rewrite_message(message: &str, file_display: &str, index: &NotebookIndex) -> String {
    let prefix = format!("{file_display}:");
    let Some(start) = message.find(prefix.as_str()) else {
        return message.to_string();
    };
    let after_prefix = &message[start + prefix.len()..];
    let digit_count = after_prefix
        .chars()
        .take_while(char::is_ascii_digit)
        .count();
    let Ok(line) = after_prefix[..digit_count].parse::<usize>() else {
        return message.to_string();
    };
    let (cell, cell_line) = translate(index, line);
    format!(
        "{before}{file_display} cell {cell}:{cell_line}{after}",
        before = &message[..start],
        after = &after_prefix[digit_count..],
    )
}

/// Translate every error and untyped-DataFrame site the linter produced -- all in
/// concatenated-source coordinates -- into notebook cell/line coordinates.
pub(crate) fn translate_result(
    errors: Vec<LintError>,
    stats: FileStats,
    file_display: &str,
    index: &NotebookIndex,
) -> NotebookCheckResult {
    let errors = errors
        .into_iter()
        .map(|e| {
            let (cell, line) = translate(index, e.line);
            NotebookLintError {
                cell,
                line,
                col: e.col,
                code: e.code,
                message: rewrite_message(&e.message, file_display, index),
                severity: e.severity,
            }
        })
        .collect();

    let untyped_sites = stats
        .untyped_sites
        .into_iter()
        .map(|s| {
            let (cell, line) = translate(index, s.line);
            NotebookUntypedSite {
                cell,
                line,
                col: s.col,
                var: s.var,
            }
        })
        .collect();

    NotebookCheckResult {
        errors,
        stats: NotebookFileStats {
            dataframes_total: stats.dataframes_total,
            dataframes_typed: stats.dataframes_typed,
            untyped_sites,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::UntypedSite;
    use ruff_notebook::Notebook;

    // Four cells: markdown, code (2 lines), markdown, code (1 line). Cell numbers
    // are 1-indexed among ALL cells (markdown included), matching ruff_notebook's
    // own convention -- so the two code cells are cell 2 and cell 4, not 1 and 2.
    fn sample_notebook() -> Notebook {
        let json = r##"{
            "cells": [
                {"cell_type": "markdown", "metadata": {}, "source": "# Title"},
                {"cell_type": "code", "metadata": {}, "execution_count": null, "outputs": [], "source": "a = 1\nb = 2"},
                {"cell_type": "markdown", "metadata": {}, "source": "text"},
                {"cell_type": "code", "metadata": {}, "execution_count": null, "outputs": [], "source": "c = 3"}
            ],
            "metadata": {},
            "nbformat": 4,
            "nbformat_minor": 5
        }"##;
        Notebook::from_source_code(json).unwrap()
    }

    #[test]
    fn test_should_translate_concatenated_line_to_cell_and_cell_relative_line() {
        // arrange
        let notebook = sample_notebook();

        // act / assert
        assert_eq!(translate(notebook.index(), 1), (2, 1)); // a = 1
        assert_eq!(translate(notebook.index(), 2), (2, 2)); // b = 2
        assert_eq!(translate(notebook.index(), 3), (4, 1)); // c = 3
    }

    #[test]
    fn test_should_fall_back_to_line_1_for_a_non_positive_line_number() {
        // arrange
        let notebook = sample_notebook();

        // act / assert
        assert_eq!(translate(notebook.index(), 0), (1, 0));
    }

    #[test]
    fn test_should_rewrite_defined_at_reference_in_message() {
        // arrange
        let notebook = sample_notebook();
        let message = "Column 'x' does not exist in S {y} (defined at analysis.ipynb:3)";

        // act
        let rewritten = rewrite_message(message, "analysis.ipynb", notebook.index());

        // assert
        assert_eq!(
            rewritten,
            "Column 'x' does not exist in S {y} (defined at analysis.ipynb cell 4:1)"
        );
    }

    #[test]
    fn test_should_leave_message_unchanged_when_no_file_reference_present() {
        // arrange
        let notebook = sample_notebook();
        let message = "Column 'x' does not exist in S {y}";

        // act
        let rewritten = rewrite_message(message, "analysis.ipynb", notebook.index());

        // assert
        assert_eq!(rewritten, message);
    }

    #[test]
    fn test_should_leave_message_unchanged_when_prefix_has_no_trailing_digits() {
        // arrange
        let notebook = sample_notebook();
        let message = "see analysis.ipynb:not-a-line-number for details";

        // act
        let rewritten = rewrite_message(message, "analysis.ipynb", notebook.index());

        // assert
        assert_eq!(rewritten, message);
    }

    #[test]
    fn test_should_translate_result_end_to_end() {
        // arrange
        let notebook = sample_notebook();
        let errors = vec![LintError {
            line: 3,
            col: 5,
            code: "unknown-column".to_string(),
            message: "Column 'z' not in S (defined at analysis.ipynb:1)".to_string(),
            severity: "error".to_string(),
        }];
        let stats = FileStats {
            dataframes_total: 2,
            dataframes_typed: 1,
            untyped_sites: vec![UntypedSite {
                line: 1,
                col: 1,
                var: "df".to_string(),
            }],
        };

        // act
        let result = translate_result(errors, stats, "analysis.ipynb", notebook.index());

        // assert
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].cell, 4);
        assert_eq!(result.errors[0].line, 1);
        assert_eq!(result.errors[0].col, 5);
        assert!(result.errors[0].message.contains("analysis.ipynb cell 2:1"));
        assert_eq!(result.stats.dataframes_total, 2);
        assert_eq!(result.stats.dataframes_typed, 1);
        assert_eq!(result.stats.untyped_sites.len(), 1);
        assert_eq!(result.stats.untyped_sites[0].cell, 2);
        assert_eq!(result.stats.untyped_sites[0].line, 1);
        assert_eq!(result.stats.untyped_sites[0].var, "df");
    }
}

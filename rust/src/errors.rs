//! Diagnostic types, per-file coverage stats, and inline suppression handling.
//!
//! Lines containing `# typedframes: ignore` suppress all diagnostics on that line;
//! `# typedframes: ignore[code1, code2]` suppresses only the listed codes. See
//! [`is_line_ignored`], which is applied as a post-processing filter after all
//! diagnostics have been collected.

use serde::{Deserialize, Serialize};

// ──────────────────────────────────────────────────────────────────────────────
// Diagnostic codes
// ──────────────────────────────────────────────────────────────────────────────

pub(crate) const CODE_UNKNOWN_COLUMN: &str = "unknown-column";
pub(crate) const CODE_RESERVED_NAME: &str = "reserved-name";
pub(crate) const CODE_UNTRACKED_DATAFRAME: &str = "untracked-dataframe";
pub(crate) const CODE_DROPPED_UNKNOWN_COLUMN: &str = "dropped-unknown-column";
pub(crate) const CODE_MISSING_COLUMN: &str = "missing-column";

// Return true if the source line at `line` (1-indexed) carries a
// `# typedframes: ignore` or `# typedframes: ignore[code]` comment.
pub(crate) fn is_line_ignored(source: &str, line: usize, code: &str) -> bool {
    let lines: Vec<&str> = source.lines().collect();
    if line == 0 || line > lines.len() {
        return false;
    }
    let line_text = lines[line - 1];
    let marker = "# typedframes: ignore";
    if let Some(pos) = line_text.find(marker) {
        let after = &line_text[pos + marker.len()..];
        // Bare ignore — suppress everything on this line
        if after.trim_start().is_empty() || after.starts_with(char::is_whitespace) {
            return true;
        }
        // Code-specific ignore: # typedframes: ignore[code1, code2]
        if after.starts_with('[') {
            if let Some(end) = after.find(']') {
                let codes: Vec<&str> = after[1..end].split(',').map(str::trim).collect();
                return codes.contains(&code);
            }
        }
    }
    false
}

/// Coverage stats for a single checked file: how many DataFrame origins the
/// linter recognized (`dataframes_total`) and how many of those resolved to a
/// known column set (`dataframes_typed`). This is informational — a low ratio
/// means the check had little to validate, not that the file has fewer
/// problems. See [`crate::linter::Linter`] for exactly what is counted.
#[derive(Debug, Serialize, Default)]
pub struct FileStats {
    pub dataframes_total: usize,
    pub dataframes_typed: usize,
    pub untyped_sites: Vec<UntypedSite>,
}

/// One DataFrame origin the linter recognized but could not resolve columns for.
///
/// The exact counterpart of `dataframes_total - dataframes_typed`: every origin
/// that bumps the denominator without bumping the numerator records a site here,
/// so `--coverage-detail=term-missing` can point at the specific assignments that
/// cost coverage rather than only reporting a ratio. Deliberately NOT derived from
/// `untracked-dataframe` diagnostics, which don't reconcile: some are retracted
/// once a call site resolves the columns cross-file (see the retraction logic in
/// `check_file`), and `[tool.typedframes] warnings = false` suppresses them
/// entirely, while coverage is counted regardless.
///
/// Line and column are 1-indexed, matching [`LintError`].
#[derive(Debug, Serialize)]
pub struct UntypedSite {
    pub line: usize,
    pub col: usize,
    /// The assigned variable name where one was available, else a generic stand-in.
    pub var: String,
}

/// The JSON payload returned by the `check_file` entry point: the diagnostics plus
/// coverage stats.
#[derive(Debug, Serialize)]
pub(crate) struct CheckFileResult {
    pub(crate) errors: Vec<LintError>,
    pub(crate) stats: FileStats,
}

/// A single diagnostic produced by the linter.
///
/// Serialises to JSON for the Python API and to the text/GitHub formats in the CLI.
/// Line and column numbers are 1-indexed to match editor conventions and the output
/// of `ruff_source_file::SourceCode::line_column` via `OneIndexed::get()`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LintError {
    /// 1-indexed source line.
    pub line: usize,
    /// 1-indexed source column.
    pub col: usize,
    /// Diagnostic code, e.g. `"unknown-column"`.  See the `CODE_*` constants.
    pub code: String,
    /// Human-readable description, optionally including a typo suggestion.
    pub message: String,
    /// `"error"` or `"warning"`.
    pub severity: String,
}

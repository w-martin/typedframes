//! Rust-based static analyser for typedframes, exposed to Python via PyO3.
//!
//! # Architecture
//!
//! The checker operates in two phases:
//!
//! 1. **Index phase** (`build_project_index`) — walks all `.py` files in the project,
//!    parses each one with `ruff_python_parser`, and extracts a lightweight symbol table
//!    (`ProjectIndex`) containing schema definitions and annotated function return types.
//!    The index is serialised to MessagePack bytes (via `rmp_serde`) and passed back to
//!    the Python caller in memory — no files are written to disk.
//!
//! 2. **Check phase** (`check_file`) — parses a single file, optionally deserialises the
//!    project index, resolves cross-file imports, and runs the [`Linter`] AST visitor.
//!    The visitor walks statements with [`Linter::visit_stmt`] (schema/variable tracking)
//!    and validates column access expressions with [`Linter::visit_expr`].  Diagnostics
//!    are returned as a JSON array of [`LintError`] objects.
//!
//! # Typo suggestions
//!
//! When a column name is not found in the known schema, the analyser computes the
//! Levenshtein edit distance between the unknown name and every known column.  If the
//! closest match is within distance ≤ 2 it is included in the diagnostic message as a
//! "did you mean?" hint.  See [`levenshtein`] and [`find_best_match`].
//!
//! # Inline suppression
//!
//! Lines containing `# typedframes: ignore` suppress all diagnostics on that line.
//! `# typedframes: ignore[code1, code2]` suppresses only the listed diagnostic codes.
//! Suppression is applied as a post-processing filter in [`Linter::check_file_internal`]
//! after all errors have been collected.

mod ast_extract;
mod config;
mod constants;
mod contract;
mod errors;
mod index;
mod linter;
mod pyapi;
mod sql;
mod typo;

pub use config::{find_project_root, is_enabled, LinterConfig};
pub use errors::{FileStats, LintError, UntypedSite};
pub use linter::Linter;

use pyo3::prelude::*;

#[pymodule]
fn _rust_checker(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(pyapi::check_file, m)?)?;
    m.add_function(wrap_pyfunction!(pyapi::build_project_index, m)?)?;
    Ok(())
}

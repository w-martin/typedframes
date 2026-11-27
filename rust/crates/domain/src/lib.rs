//! Rust-based static analyser for typedframes: column inference and checking for pandas and polars.
//!
//! # Architecture
//!
//! The checker operates in two phases:
//!
//! 1. **Index phase** (`index::build_index_cached`) — walks all `.py` files in the project,
//!    parses each one with `ruff_python_parser`, and extracts a lightweight symbol table
//!    (`ProjectIndex`) containing schema definitions and annotated function return types.
//!    Checking a single file instead uses `index::build_single_file_index_cached`, which
//!    produces the same `ProjectIndex` from a much narrower file set: just the checked file
//!    plus the project-local modules and external packages its own imports actually reach.
//!    Per-file results are cached on disk and reused while a file's source is unchanged.
//!
//! 2. **Check phase** ([`engine`]) — parses each file, resolves cross-file imports against the
//!    index, and runs the [`Linter`] AST visitor in parallel. The visitor walks statements
//!    with [`Linter::visit_stmt`] (schema/variable tracking) and validates column access
//!    expressions with [`Linter::visit_expr`]. Diagnostics come back as [`LintError`] values.
//!
//! The `typedframes_cli` crate is the command line built on this one.
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

pub mod analysis;
mod ast_extract;
mod call_contracts;
mod call_usage;
mod column_consuming_methods;
mod column_usage;
pub mod config;
mod constants;
mod contract;
mod dict_frames;
pub mod edits;
pub mod errors;
mod fast_hash;
mod frame_ops;
mod helpers;
pub mod index;
mod linter;
pub mod notebook;
pub mod parallel;
mod phrase;
mod polars_names;
pub mod ports;
mod query_names;
pub mod report;
mod report_minimize;
mod report_names;
mod returned_loads;
mod self_frames;
mod sql;
mod static_eval;
pub mod suggest;
mod typo;

pub use config::{find_project_root, is_enabled, LinterConfig};
pub use errors::{FileStats, LintError, UntypedSite};
pub use linter::Linter;

//! Suggestions for an untyped DataFrame origin, built from how the rest of its scope
//! uses it (see `column_usage`).
//!
//! The one suggestion is a column selection, which gives the checker an exact schema:
//! `usecols=`/`columns=` added to a plain `read_csv`/`read_parquet` call, or else
//! `df = df[["a", "b"]]` (pandas) / `df = df.select("a", "b")` (polars) on its own line
//! after the load. It rewrites real code, so it is made only when every use of the
//! frame is visible and is a read of a named column; anything else is withheld with a
//! reason, since dropping a column the program still reads breaks it at runtime.
//!
//! Output is byte-range edits plus a rendered diff; `apply` is what writes them.

use crate::column_usage::{ColumnUsage, OtherUse};
use crate::constants::STDLIB_MODULES;
use ruff_python_ast::{self as ast, Expr, ModModule, Stmt};
use ruff_text_size::Ranged;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

mod diff;
mod final_constant;
mod final_import;
mod libraries;
mod local_library;
mod selection;

pub(crate) use diff::*;
pub(crate) use final_constant::*;
pub(crate) use final_import::*;
pub(crate) use libraries::*;
use local_library::*;
pub(crate) use selection::*;

/// One byte-range replacement against the ORIGINAL source (`start == end` inserts).
#[derive(Debug, Clone, Serialize)]
pub struct Edit {
    pub start: usize,
    pub end: usize,
    pub replacement: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Suggestion {
    /// The site's stable two-word name; filled in once all of a file's sites are known.
    pub phrase: String,
    pub kind: &'static str,
    pub line: usize,
    pub col: usize,
    pub var: String,
    /// `"safe"` when nothing about the change needs a human's judgement, `"review"`
    /// when it is probably right but depends on something this scan can't see (see
    /// `notes`).
    pub tier: &'static str,
    pub columns: Vec<String>,
    pub reason: String,
    pub notes: Vec<String>,
    pub edits: Vec<Edit>,
    pub diff: String,
    /// In a notebook, the 1-indexed cell and the line within it; `line` stays the position in
    /// the notebook's concatenated source, which is what edits and selections are keyed on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cell: Option<(usize, usize)>,
}

/// A site that got no suggestion, and why.
#[derive(Debug, Clone, Serialize)]
pub struct Withheld {
    pub phrase: String,
    pub line: usize,
    pub col: usize,
    pub var: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cell: Option<(usize, usize)>,
}

pub(crate) struct Origin<'a> {
    pub(crate) libraries: &'a LibraryContext,
    /// The variable's own annotation, when it has one (`df: pd.DataFrame = ...`).
    pub(crate) annotation: Option<&'a Expr>,
    pub(crate) source: &'a str,
    pub(crate) display_path: &'a str,
    /// The expression assigned to the variable (`None` for a bare annotation).
    pub(crate) value: Option<&'a Expr>,
    pub(crate) var: &'a str,
    pub(crate) line: usize,
    pub(crate) col: usize,
    /// Byte range of the whole assignment statement.
    pub(crate) stmt_start: usize,
    pub(crate) stmt_end: usize,
    /// A local of a function rather than a module-level name. A module-level frame can
    /// be imported by other modules, which this scan can't see.
    pub(crate) function_local: bool,
    /// Local names (and parameters) of the enclosing function that are certainly an
    /// instance of a class, by class name: `svc = Service()`, `def f(svc: Service)`.
    pub(crate) receivers: &'a HashMap<String, String>,
    /// Every column name any frame in the project is known to have, for spotting a misspelling.
    pub(crate) known_columns: &'a [String],
    /// The function or module body the origin is in, for the library of a local it is built from.
    pub(crate) scope_body: &'a [Stmt],
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Library {
    Pandas,
    Polars,
}

impl Library {
    /// The name stored in the index.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Library::Pandas => "pandas",
            Library::Polars => "polars",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Library> {
        match name {
            "pandas" => Some(Library::Pandas),
            "polars" => Some(Library::Polars),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum ModuleKind {
    Library(Library),
    Connectorx,
}

/// What a file's own imports and function signatures say about which library a frame
/// belongs to. Built once per file.
#[derive(Default, Debug)]
pub(crate) struct LibraryContext {
    // Local name of an imported module: `import pandas as pd` -> pd.
    modules: HashMap<String, ModuleKind>,
    // Local name of something imported from pandas/polars: `from pandas import read_csv`.
    names: HashMap<String, Library>,
    // Functions whose return annotation says pandas or polars, by bare name: this file's
    // own, plus those imported from other project files (from the index).
    functions: HashMap<String, Library>,
    // Methods whose return annotation says pandas or polars, by `(class, method)`: a call on
    // a receiver known to be an instance of the class. This file's classes, plus those it
    // imports from other project files (from the index).
    class_methods: HashMap<(String, String), Library>,
}

#[cfg(test)]
mod tests;

//! The core AST-visitor engine: schema tracking, mutation detection,
//! method-chain analysis, load-function dispatch, and diagnostic emission.
//!
//! Mutually dependent with [`crate::index`]: the linter calls back into indexing
//! to resolve cross-file symbols, and indexing reuses the linter to scan files.

use crate::config::LinterConfig;
use crate::constants::{
    CONNECTORX_MODULES, FEAST_RETRIEVAL_METHODS, LOAD_FUNCTIONS, LOAD_MODULES, OPEN_FRAME_MARKER,
    RESERVED_METHODS, ROW_PASSTHROUGH_METHODS, SCHEMA_PRESERVING_METHODS, SQL_FINALIZE_METHODS,
    SQL_LOAD_FUNCTIONS,
};
use crate::errors::{
    is_line_ignored_in, CODE_ASSUMED_CONSTANT, CODE_DROPPED_UNKNOWN_COLUMN, CODE_LIKELY_TYPO,
    CODE_MISSING_COLUMN, CODE_READ_BEFORE_ASSIGNMENT, CODE_RESERVED_NAME, CODE_UNKNOWN_COLUMN,
    CODE_UNRESOLVED_SCHEMA, CODE_UNTRACKED_DATAFRAME, CODE_UNVERIFIABLE_COLUMN,
};
use crate::errors::{DataFrameCallSite, LegEvent, LintError, TypedSite, UntypedSite};
use crate::frame_ops::{
    apply_edits, class_chain, classify_rhs, column_write_target, describe_rhs,
    dynamic_column_write_receiver, inplace_drop_columns, inplace_literal, inplace_rename_mapping,
    keyword, merged_columns, resolve_delegates, row_insert_receiver, summarize_helper, ColumnEdit,
    FnKey, HelperSummary, InplaceColumns, Kind, Merged,
};
use crate::index::{resolve_module_file_with_root, IndexEntry, ProjectIndex};
use crate::typo::find_best_match;
use crate::{
    ast_extract, call_contracts, column_usage, contract, helpers, query_names, sql, static_eval,
    suggest,
};
use ruff_python_ast::visitor::{self as ast_visitor, Visitor};
use ruff_python_ast::{self as ast, Expr, ModModule, Stmt};
use ruff_python_parser::{parse, parse_module, Mode, ParseOptions};
use ruff_source_file::{LineIndex, SourceCode};
use ruff_text_size::Ranged;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

mod annotations;
mod arguments;
mod arguments_rows_joins;
mod assign_targets;
mod ast_helpers;
mod call_sites;
mod classes;
mod collectors;
mod column_assignments;
mod column_checks;
mod column_writes;
mod combining;
mod contracts;
mod cross_file;
mod derived_columns;
mod dict_list_bindings;
mod entry;
mod error_ranking;
mod expressions;
mod feast;
mod forwarders;
mod function_returns;
mod functions;
mod groupby;
mod helper_calls;
mod hints;
mod import_symbols;
mod index_bindings;
mod inplace;
mod load_assignments;
mod load_columns;
mod module_imports;
mod open_after;
mod open_derivations;
mod open_frames;
mod open_writes;
mod origin_scan;
mod origins;
mod polars_ops;
mod reach_visitors;
mod registration;
mod renames_drops;
mod reports;
mod returned;
mod returned_ops;
mod schema_classes;
mod schema_constructors;
mod selections;
mod site_counts;
mod stale_legs;
mod state;
mod statements;
mod string_bindings;
mod tracking;
mod untyped_sites;

use arguments_rows_joins::*;
use ast_helpers::*;
use call_sites::*;
pub(crate) use collectors::*;
use dict_list_bindings::*;
use error_ranking::*;
pub(crate) use forwarders::*;
use hints::*;
use inplace::*;
use module_imports::*;
use open_writes::*;
pub(crate) use origins::*;
use reach_visitors::*;
use registration::*;
use returned_ops::*;
pub use state::*;
pub(crate) use string_bindings::*;
pub(crate) use tracking::*;

// Which family of load a call belongs to — used only to phrase the
// `untracked-dataframe` hint appropriately when extraction fails. Not persisted anywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoadKind {
    File,
    Sql,
    // A SQLAlchemy Core `select(...)` statement (or a `Name` bound to one) rather than
    // SQL text — see `extract_orm_select_columns`.
    Orm,
}

// A recognized case-fold of an *already-known* column set — e.g. a connector-specific
// package that queries Snowflake (columns come back upper-cased, per `sql_dialect`
// folding) and then lower-cases them all before returning. Deliberately narrow: this
// matches two specific, literal AST shapes (`.rename(columns=str.lower)` /
// `df.columns = df.columns.str.lower()`, and their `.upper()` counterparts) rather
// than attempting to evaluate arbitrary user-defined transform functions, which is not
// possible in general for a static checker — any custom function (`my_pkg.normalize`,
// a lambda, anything data-dependent) is invisible to this and passes the base schema
// through unchanged, neither erroring nor folding it. See docs/usage.md's "Supported
// column-set transforms" section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaseFold {
    Lower,
    Upper,
}

impl CaseFold {
    fn apply(self, s: &str) -> String {
        match self {
            CaseFold::Lower => s.to_lowercase(),
            CaseFold::Upper => s.to_uppercase(),
        }
    }
}

// What runs after a nested block: `extra` (blocks only this block can fall into), the rest of
// the enclosing block, then everything after that.
fn following<'a>(extra: &[&'a [Stmt]], rest: &'a [Stmt], tails: &[&'a [Stmt]]) -> Vec<&'a [Stmt]> {
    let mut chain: Vec<&[Stmt]> = extra.to_vec();
    chain.push(rest);
    chain.extend_from_slice(tails);
    chain
}

// The function a body belongs to: its name, for finding its callers, and the locals and
// parameters that are certainly an instance of a class (see `receiver_classes`).
struct FunctionContext<'a> {
    name: Option<&'a str>,
    receivers: HashMap<String, String>,
}

impl FunctionContext<'_> {
    fn module() -> Self {
        FunctionContext {
            name: None,
            receivers: HashMap::new(),
        }
    }
}

// The function or module body being scanned, as every origin inside it sees it.
struct UsageScope<'a> {
    libraries: &'a suggest::LibraryContext,
    helpers: &'a dyn column_usage::HelperResolver,
    body: &'a [Stmt],
    by_position: &'a HashMap<(usize, usize), &'a str>,
    in_function: bool,
    // The function the body belongs to, for finding its callers.
    function_name: Option<&'a str>,
    // Locals and parameters of the function that are certainly an instance of a class.
    receivers: HashMap<String, String>,
}

// What one pass over a file's scopes learns about its untyped sites: their downstream
// usage, and (only when `Linter::collect_suggestions` is on) a suggestion or the reason
// there isn't one.
#[derive(Default)]
struct SiteScanResults {
    usage: HashMap<(usize, usize), column_usage::ColumnUsage>,
    outcomes: Vec<((usize, usize), Result<suggest::Suggestion, String>)>,
}

// A function parameter that, when a caller passes a *literal* argument for it, lets the
// checker resolve a Feast `features=<param>` call inside this function's body and
// validate the specific column accesses the body makes on its result — independently
// per call site, since different callers can pass different (or no) literal for the
// same parameter. Detected by `find_param_governed_feast_template`, consumed by
// `resolve_param_governed_call_sites` — see that function's doc comment for how a
// diagnostic ends up attributed to the *call site* rather than the access line inside
// this function's own body (which stays a single, caller-independent AST location).
//
// Deliberately narrow, matching this checker's general philosophy for anything
// heuristic: only the *chained* Feast form (`df = store.get_historical_features(...,
// features=<param>).to_df()`) as a direct statement in the function's own top-level
// body is recognized — not the split form (`job = store.get_...(...)`), not a
// parameter buried in nested control flow, and not SQL-text-argument governance (a
// parameter feeding `pd.read_sql(<param>, conn)` and similar). Extending to those is
// possible but not implemented here.
#[derive(Debug, Clone)]
pub(crate) struct ParamGovernedTemplate {
    pub(crate) param_name: String,
    // Index among posonlyargs+args (kwonlyargs excluded — those can't be passed
    // positionally at a call site) so a call site matching by position, not just by
    // keyword, still resolves.
    pub(crate) param_index: usize,
    pub(crate) full_feature_names: bool,
    pub(crate) accesses: Vec<ParamGovernedAccess>,
    // Position of the governing `df = store.get_...(...).to_df()` statement itself —
    // used only to remove the untracked-dataframe warning `register_feast_dataframe`
    // already pushed for it during the normal body walk (which runs before this
    // template is even detected). That local, in-isolation "columns unknown at lint
    // time" framing is simply wrong once we know this call is resolvable by tracing
    // callers — the real answer moved to the call sites, not "unknown".
    pub(crate) governing_line: usize,
    pub(crate) governing_col: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct ParamGovernedAccess {
    pub(crate) line: usize,
    pub(crate) col: usize,
    pub(crate) column: String,
}

// An `x = call(...)` assignment where `call` wasn't recognized as any DataFrame
// origin, but `x` is later used the way a DataFrame is used. `name` is the raw
// identifier at the call site: the receiver for an attribute call (`mod.func(...)` ->
// "mod"), or the callee itself for a bare call (`func(...)` -> "func") — `index.rs`
// resolves this name against that same file's own `module_aliases`/`imports` maps
// (whichever applies, per `is_attribute_call`) to find the module it came from. See
// `Linter::unresolved_dataframe_shaped_calls`.
#[derive(Debug, Clone)]
pub(crate) struct UnresolvedDataframeShapedCall {
    pub(crate) name: String,
    pub(crate) is_attribute_call: bool,
    // Where the variable is assigned, its name, and (once named) its two-word phrase, for
    // `report --outcome untracked`.
    pub(crate) line: usize,
    pub(crate) col: usize,
    pub(crate) var: String,
    pub(crate) phrase: String,
    // What kind of call it is, in words that name nothing from the user's code.
    pub(crate) callee: &'static str,
}

/// What an assigned call turned out to merge or concatenate, found while dispatching on its callee.
#[derive(Default)]
struct MergeOutcome<'a> {
    is_merge_or_concat: bool,
    schema: Option<Vec<String>>,
    call: Option<&'a ast::ExprCall>,
}

// (feature_cols, entity_cols) resolved for a Feast retrieval -- see
// Linter::register_feast_dataframe.
type FeastColumns = (Option<Vec<String>>, Option<Vec<String>>);

const MAX_SQL_FILE_BYTES: u64 = 256 * 1024;

const MAX_SQL_FILE_READS_PER_FILE: u32 = 32;

const GROUPBY_SELECTION_AGGREGATE_REDUCTIONS: &[&str] = &[
    "mean", "sum", "median", "min", "max", "first", "last", "std", "var", "count", "nunique",
    "size", "prod",
];

const REDUCTIONS: &[&str] = &[
    "mean", "sum", "median", "min", "max", "first", "last", "std", "var", "count", "nunique",
    "prod",
];

const KEEPS_NAME: &[&str] = &[
    "mean",
    "sum",
    "median",
    "min",
    "max",
    "first",
    "last",
    "std",
    "var",
    "count",
    "nunique",
    "prod",
    "fillna",
    "astype",
    "abs",
    "round",
    "shift",
    "diff",
    "cumsum",
    "ffill",
    "bfill",
    "clip",
    "dropna",
    "sort_values",
    "sort_index",
    "head",
    "tail",
    "copy",
];

const MAX_COMBINATIONS: usize = 2_000;

#[cfg(test)]
mod tests;

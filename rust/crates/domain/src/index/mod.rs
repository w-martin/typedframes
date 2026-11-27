//! Cross-file project index: walks the project's `.py` files, extracts a
//! lightweight symbol table, and resolves transitive column requirements
//! across function calls.
//!
//! Mutually dependent with [`crate::Linter`]: indexing reuses the linter to scan
//! individual files, and the linter calls back here to resolve cross-file symbols.

use crate::ast_extract;
use crate::config::{load_linter_config, LinterConfig};
use crate::constants::DEFAULT_EXCLUDED_DIRS;
use crate::errors::{LintError, CODE_UNKNOWN_COLUMN, CODE_UNTRACKED_DATAFRAME};
use crate::frame_ops::{ColumnEdit, FnKey, HelperSummary};
use crate::linter::{
    annotated_param_origins, collect_self_attr_origins, Linter, ParamGovernedTemplate,
    SelfAttrOrigin,
};
use crate::ports::FileSystem;
use ruff_python_ast::{self as ast, Expr, Stmt};
use ruff_python_parser::parse_module;
use ruff_source_file::{LineIndex, SourceCode};
use ruff_text_size::Ranged;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

mod build;
mod cache;
mod call_sites;
mod discover;
mod feast_eval;
mod governed;
mod helper_resolver;
mod module_exports;
mod resolve;
mod single_file;
mod source;
mod transitive;

pub use build::*;
pub(crate) use cache::*;
pub(crate) use call_sites::*;
pub(crate) use discover::*;
pub(crate) use feast_eval::*;
pub(crate) use governed::*;
use helper_resolver::*;
pub(crate) use module_exports::*;
pub(crate) use resolve::*;
pub use single_file::*;
pub(crate) use source::*;
pub(crate) use transitive::*;

// Return-type and parameter-contract information extracted from a function definition.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct IndexFunction {
    pub(crate) returns_schema: String, // BaseSchema subclass name, e.g. "OrderSchema"; empty if none
    // "pandas" or "polars" when the return annotation names that library's frame type;
    // empty otherwise. Lets a caller in another file tell which library a call returns.
    pub(crate) returns_library: String,
    // What the function does with each parameter whose every use is a read of a named column,
    // so a caller passing a frame to it knows which columns it needs. Absent for a function
    // that does anything else with its parameters.
    #[serde(default)]
    pub(crate) param_usage: Vec<crate::column_usage::ParamUsage>,
    // Columns required on the function's first parameter. Starts as the columns
    // accessed *directly* in the function body; resolve_transitive_requires (run once,
    // project-wide, in build_index_internal) folds in the requirements of every
    // function this one delegates its own parameter to, so by the time an importer
    // reads this field it already reflects the full transitive union.
    pub(crate) requires: Vec<String>,
    pub(crate) def_line: usize, // line the function is defined on, for origin messages
    // Names of functions called with a tainted variable (the parameter itself, or a
    // variable derived from it) as their first positional argument. Consumed only by
    // resolve_transitive_requires; not needed once `requires` has been resolved.
    // `#[serde(skip)]`: this is pure intermediate state, dead the moment
    // build_index_internal finishes — it must never reach the serialised
    // ProjectIndex that check_file deserialises once per file. On a large,
    // delegate-heavy project (helper functions that just forward their first
    // argument are extremely common Python, whether or not they touch a
    // DataFrame) these lists dominate the index's serialised size, and paying
    // that deserialisation cost once per file turns "more thorough analysis"
    // into an O(files) cost repeated per file, i.e. O(files^2) overall.
    #[serde(skip)]
    pub(crate) delegates: Vec<String>,
    // Schema name from the first parameter's annotation, e.g. "ReportSchema" from
    // `Annotated[pl.DataFrame, ReportSchema]`; empty if the parameter isn't
    // schema-annotated. Captured at index time regardless of whether the schema is
    // resolvable yet (it may be defined in a third file) — resolve_transitive_requires
    // resolves it project-wide and overrides `requires` with the authoritative column
    // list once every file's schemas are known, superseding the heuristic body-scan.
    // `#[serde(skip)]`: same reasoning as `delegates` above — dead weight past
    // resolve_param_schema_requires, must not be serialised into ProjectIndex.
    #[serde(skip)]
    pub(crate) param_schema_name: String,
    // A parameter that governs a recognized Feast features= call inside this function's
    // body — see ParamGovernedTemplate. `#[serde(skip)]`: consumed entirely within
    // build_index_internal by resolve_param_governed_call_sites, which produces the
    // (per-call-site) diagnostics that actually get serialised into ProjectIndex; this
    // template itself never needs to survive past that, same reasoning as `delegates`
    // and `param_schema_name` above.
    #[serde(skip)]
    pub(crate) param_governed: Option<ParamGovernedTemplate>,
    // Whether every read behind `requires` runs on each call: the body (and each body it
    // delegates to) is straight-line, or a schema annotation gave the columns. Only then do
    // call sites check against `requires`. Build-time only, like `delegates`.
    #[serde(skip)]
    pub(crate) straight: bool,
    #[serde(skip)]
    pub(crate) requires_certain: bool,
    // What a call to this (module-level) function does to the frame it is handed -- see
    // `frame_ops::summarize_helper`. Already resolved within this file, so an importer can
    // apply it without seeing the body. Absent for a function that does neither.
    #[serde(default)]
    pub(crate) helper_summary: Option<HelperSummary>,
}

// The statement-level calls a file makes, recorded while the file is parsed so that finding
// which functions are called does not parse every file a second time: the callee names, and
// the `self.<attr>.<method>()` calls with what `<attr>` was assigned. Only needed until
// `resolve_called_functions` has run; it is then dropped, so the index sent on does not carry it.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct CallSites {
    pub(crate) names: Vec<String>,
    pub(crate) self_calls: Vec<(SelfAttrOrigin, String)>,
}

impl CallSites {
    pub(crate) fn is_empty(&self) -> bool {
        self.names.is_empty() && self.self_calls.is_empty()
    }
}

// Symbol table for a single `.py` file, stored inside ProjectIndex.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct IndexEntry {
    pub(crate) schemas: HashMap<String, Vec<String>>, // schema name -> column list
    // Named schema name -> (file it's defined in, definition line) -- see Linter's
    // own `schema_locations` field for why this is tracked separately from `schemas`.
    #[serde(default)]
    pub(crate) schema_locations: HashMap<String, (String, usize)>,
    pub(crate) functions: HashMap<String, IndexFunction>, // function name -> return type info
    pub(crate) exports: Vec<String>, // names in __all__, for wildcard-import resolution
    pub(crate) imports: HashMap<String, String>, // imported name -> dotted module it came from (`from X import Y`)
    // Local alias -> dotted module name, from plain `import module [as alias]`
    // statements. Lets resolve_delegate_target and load_cross_file_symbols follow
    // attribute-style calls (`transforms.enrich(df)`) back to the module they came
    // from, the same way `imports` does for `from transforms import enrich`.
    pub(crate) module_aliases: HashMap<String, String>,
    // Class name -> {method name -> return schema}, a direct copy of this file's own
    // `Linter::class_methods` (see its doc comment for the value convention --
    // OPEN_FRAME_MARKER for a bare pd.DataFrame/pl.DataFrame return). Lets
    // `Linter::import_name` resolve `self.<attr>.<method>(...)` when `<attr>`'s class
    // is imported from ANOTHER file -- first-party, or a `py.typed` external package
    // once traced (see `package_declares_py_typed`) -- the same way `functions`
    // already lets a plain imported function resolve. No project-wide union needed
    // the way `all_schemas` has one: a class is looked up directly in the file it
    // resolves to via `resolve_module_file`, not through a third file's indirection.
    #[serde(default)]
    pub(crate) class_methods: HashMap<String, HashMap<String, String>>,
    // Class name -> {method name -> "pandas" | "polars"}, for methods whose return annotation
    // names that library's frame type. Lets a file that imports the class tell which library
    // `instance.method()` or an inherited `self.method()` returns; `class_methods` above can't,
    // as it marks a bare frame return the same way for both.
    #[serde(default)]
    pub(crate) class_method_libraries: HashMap<String, HashMap<String, String>>,
    // Base class names (as written) of every class defined in this file, keyed by class
    // name -- a direct copy of `Linter::class_bases`. Every class gets an entry, even one
    // with no bases, so presence of a key is "this class is defined in this file". Same-file
    // only; a base defined elsewhere is resolved by `resolve_class_file` using `imports`/
    // `module_aliases` below, the same way `class_methods` above is reached cross-file.
    #[serde(default)]
    pub(crate) class_bases: HashMap<String, Vec<String>>,
    // Class name -> {method name -> raw helper summary}, this file's own contribution --
    // a direct copy of `Linter::helper_summaries`' `Some(class)` entries, computed with
    // `Linter::preserve_unresolved_delegates` set (see its doc comment). Finished into each
    // method's actual cross-file, inheritance-aware effect by
    // `resolve_transitive_helper_summaries`, the `class_bases`/self-method counterpart to
    // `functions[_].helper_summary`'s plain-function resolution.
    #[serde(default)]
    pub(crate) class_helper_summaries: HashMap<String, HashMap<String, HelperSummary>>,
    // Statically known constants and evaluable functions, for importing files to fold.
    #[serde(default)]
    pub(crate) eval: crate::static_eval::ModuleExports,
    // Names of other modules this file may change, which their importers must not fold;
    // applied (and emptied) by `finalise_index`.
    #[serde(default)]
    pub(crate) foreign_writes: Vec<crate::static_eval::ForeignWrite>,
    // Every module this file's top-level imports may load, as `(leading dots, dotted name)`;
    // `from a import b` lists both `a` and `a.b`, since `b` may be a submodule.
    #[serde(default)]
    pub(crate) import_targets: Vec<(u32, String)>,
    // Column requirements per parameter of this file's functions (`name`) and methods
    // (`Class.name`), for those with any; see `call_contracts`.
    #[serde(default)]
    pub(crate) contracts: HashMap<String, crate::call_contracts::FuncContract>,
    // Parameters handed on to a function of another file, by contract key; resolved one
    // level deep by `resolve_contract_forwards`. Empty in a finished index.
    #[serde(default)]
    pub(crate) contract_forwards: Vec<(String, crate::call_contracts::Forward)>,
    // Method names of each class, and which classes are protocols, for `method_agreement`.
    // Empty in a finished index.
    #[serde(default)]
    pub(crate) class_method_names: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub(crate) protocol_classes: Vec<String>,
    // Last: skipped when empty, and the cache encodes fields by position.
    // See `CallSites`. Empty in a finished index.
    #[serde(default, skip_serializing_if = "CallSites::is_empty")]
    pub(crate) call_sites: CallSites,
}

// In-memory cross-file symbol index.
// Serialised as MessagePack so it can be held in Python memory and passed to check_file
// without any intermediate disk I/O.  The version field allows future format migrations.
#[derive(Serialize, Deserialize)]
pub struct ProjectIndex {
    pub(crate) version: u32, // format version, currently always 1
    pub(crate) files: HashMap<String, IndexEntry>, // absolute file path -> IndexEntry
    // Project-wide schema name -> column list, unioned across every file's `schemas`
    // map. Computed once here in build_index_internal so that per-file lookups (see
    // Linter::load_cross_file_symbols, called once per file by `check_file`) are a
    // single field read instead of a rebuild-by-iterating-every-file — the latter
    // turned a per-file O(1) lookup into an O(files) one, i.e. O(files^2) for a whole
    // project check. A schema may be defined in a THIRD file — neither the function's
    // own file nor the one importing it — so this must stay project-wide rather than
    // scoped to a single IndexEntry.
    pub(crate) all_schemas: HashMap<String, Vec<String>>,
    // Project-wide schema name -> (file its class is defined in, definition line),
    // unioned the same way as `all_schemas` above and for the same reason -- see
    // Linter's own `schema_locations` field.
    #[serde(default)]
    pub(crate) all_schema_locations: HashMap<String, (String, usize)>,
    // Diagnostics resolved at the *call site*, keyed by the calling file's absolute
    // path — populated once, project-wide, by resolve_param_governed_call_sites, and
    // spliced into a file's own diagnostics by check_file when that file is checked.
    // Attributing these to the call site (rather than the access line inside the
    // callee's own body, which stays one caller-independent AST location) is what lets
    // two different callers passing two different literals for the same parameter
    // resolve completely independently, instead of the callee's line needing two
    // different diagnoses at once.
    #[serde(default)]
    pub(crate) call_site_errors: HashMap<String, Vec<LintError>>,
    // (file, func_name) pairs where at least one call site project-wide was actually
    // SEEN for a param-governed Feast call — regardless of whether that call site's
    // argument resolved to a literal or not. Consulted by check_file to decide whether
    // the intra-function untracked-dataframe warning register_feast_dataframe pushes
    // for that same call is stale (retract it — the real answer, resolved or not,
    // lives at the call sites now, each with its own diagnostic: OK, an unknown-column
    // error, or its own untracked-dataframe info) or still the only real signal (leave
    // it — no call site anywhere in the project was ever traced back to this function
    // at all, e.g. a fully dynamic dispatch, so there's nowhere else to put the
    // warning and it would be wrong to silently report nothing).
    #[serde(default)]
    pub(crate) resolved_governed: std::collections::HashSet<(String, String)>,
    // Defining file -> bare function/method names confirmed called from a direct
    // `x = name(...)` call site SOMEWHERE in the project (any file, including the
    // defining file itself -- redundant with what same-file confirmation already
    // finds there, but harmless: `Linter::functions_called` is a plain HashSet, and
    // inserting an already-present name is a no-op).
    //
    // This is what lets `Linter::pending_passthrough_reversals` (see its doc comment
    // in linter.rs) confirm a pass-through function's count across a file boundary --
    // `loaders.py` defines `load_orders` with a bare `return df`; without this, only
    // `pipeline.py`'s own call site would ever confirm it (via cross-file
    // `self.functions` resolution), while `loaders.py`'s OWN per-file check has no way
    // to know anyone calls it, so it would keep counting its internal origin too --
    // the exact cross-file double-count this field closes. Seeded into each file's
    // `Linter::functions_called` by `load_cross_file_symbols`, BEFORE that file's own
    // statement walk runs, so the existing end-of-walk resolution loop
    // (`check_parsed_module`) picks up a cross-file confirmation with no change to the
    // reversal mechanism itself.
    //
    // Scoped to bare-name calls only (`resolve_delegate_target`'s own resolution
    // capability, the same one `resolve_param_governed_call_sites` already relies on)
    // -- a cross-file `self.<attr>.<method>()` call site is NOT covered (would need
    // `current_self_attrs`-equivalent tracking reconstructed for every file in this
    // lightweight re-scan, not just the `Linter`-based `index_file` pass), so that
    // shape's cross-file confirmation remains a known, narrower follow-up gap; its
    // SAME-FILE confirmation is unaffected and already works.
    #[serde(default)]
    pub(crate) called_functions: HashMap<String, std::collections::HashSet<String>>,
    // Function or method name -> what every call to it in the project does with the frame it
    // returns, for the names where every call is seen and only reads named columns of the
    // result. Absent means unknown, never "no columns". Empty unless the index covers the
    // whole project: an index built around one file's imports never sees its callers.
    #[serde(default)]
    pub(crate) call_usage: HashMap<String, CallUsage>,
    // Method name -> the requirement every class of the project defining it agrees on (with
    // a description of where), for calls through a protocol-typed receiver. Only methods
    // with a requirement; empty unless the index covers the whole project.
    #[serde(default)]
    pub(crate) method_agreement: HashMap<String, (crate::call_contracts::FuncContract, String)>,
    // Class name -> (method, file) for each method a class of the project defines over one
    // it inherits from that class, matched by name.
    #[serde(default)]
    pub(crate) overridden: HashMap<String, Vec<(String, String)>>,
    // Resolved re-exports per module, so a module is walked once per run, not once per
    // importer and per path to it. Never serialised.
    #[serde(skip)]
    pub(crate) exports_memo: ExportsMemo,
    #[serde(skip)]
    pub(crate) package_roots: crate::config::PackageRoots,
}

type ExportsKey = (String, usize, PathBuf);

/// What `linter::module_exports` computed for (file, remaining re-export depth, project
/// root). A pure function of the index, so concurrent fills agree.
#[derive(Default)]
pub(crate) struct ExportsMemo(
    std::sync::RwLock<
        HashMap<ExportsKey, Option<std::sync::Arc<crate::static_eval::ModuleExports>>>,
    >,
);

impl ExportsMemo {
    pub(crate) fn get(
        &self,
        key: &ExportsKey,
    ) -> Option<Option<std::sync::Arc<crate::static_eval::ModuleExports>>> {
        self.0.read().ok()?.get(key).cloned()
    }

    pub(crate) fn insert(
        &self,
        key: ExportsKey,
        value: Option<std::sync::Arc<crate::static_eval::ModuleExports>>,
    ) {
        if let Ok(mut memo) = self.0.write() {
            memo.insert(key, value);
        }
    }
}

/// The columns read from the result of every call to a function, across the project.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct CallUsage {
    pub(crate) columns: Vec<String>,
    /// Calls seen, counting the defining file's own.
    pub(crate) calls: usize,
}

#[cfg(test)]
mod tests;

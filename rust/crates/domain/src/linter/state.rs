use super::*;

#[derive(Default)]
pub struct Linter {
    // Where imported modules and `.sql` files are read from.
    pub(crate) fs: crate::ports::FileSystemHandle,
    pub(crate) schemas: HashMap<String, Vec<String>>,
    pub(crate) variables: HashMap<String, (String, usize)>, // var_name -> (schema_name, defined_at_line)
    pub(crate) functions: HashMap<String, String>, // func_name -> schema_name (from return type)
    // A function of this file returning a tuple: the schema of each element that is a frame
    // with known columns.
    pub(super) tuple_functions: HashMap<String, Vec<Option<String>>>,
    pub(crate) schema_origins: HashMap<String, String>, // inferred schema name -> "func (path:line)"
    // Named schema (BaseSchema subclass / SQLAlchemy declarative model) name -> (file
    // its class is defined in, definition line). Populated wherever `self.schemas`
    // gets a named-schema entry (see Stmt::ClassDef handling), and carried cross-file
    // by load_cross_file_symbols/import_name -- consulted by schema_display so an
    // unknown-column message points at the schema's actual class definition, which can
    // be an entirely different file than wherever the erroring variable was bound.
    pub(crate) schema_locations: HashMap<String, (String, usize)>,
    pub(crate) requires: HashMap<String, (Vec<String>, usize)>, // func_name -> (direct required cols on 1st param, def line)
    // Functions in `requires` whose body is straight-line (`call_contracts::is_straight_line`).
    pub(crate) straight_line_functions: std::collections::HashSet<String>,
    pub(crate) delegates: HashMap<String, Vec<String>>, // func_name -> names called with its own (tainted) param forwarded
    pub(crate) param_schema_names: HashMap<String, (String, usize)>, // func_name -> (first-param schema annotation name, def line)
    pub(crate) line_index: Option<LineIndex>,
    pub(crate) source: String,
    pub(crate) file_display: String, // absolute-ish path of the file currently being linted
    // Coverage counters: how many DataFrame origins the linter positively
    // identified (a recognized load call, `Schema.from_pandas`, or an
    // assignment from a function known to return a schema) vs. how many of
    // those origins resolved to a known column set. This is a "did we have
    // enough information" signal for the user, not a validation result — a
    // plain, unresolved function call assigned to a variable is deliberately
    // NOT counted here, since the linter has no way to tell whether it
    // returns a DataFrame at all; counting it would inflate the denominator
    // with unrelated calls.
    pub dataframes_total: usize,
    // An origin in `unresolved_schemas` does NOT bump this counter -- no
    // `unknown-column` verdict is possible without a concrete column list, so
    // every access on it raises `unverifiable-column` instead. Falls into
    // `untyped_sites` like any other unresolved DataFrame (e.g. unparseable SQL).
    pub dataframes_typed: usize,
    // Annotated frames whose schema name resolved to no class; also in `untyped_sites`.
    pub dataframes_unresolved: usize,
    // Every origin counted in `dataframes_total` that did NOT resolve to a concrete
    // column set. Kept in step with the counters at each counting site rather than
    // reconstructed later.
    pub untyped_sites: Vec<UntypedSite>,
    // Downstream usage observed for each `untyped_sites` entry whose own origin
    // statement sits at the top level of its function/module body -- see
    // `column_usage`'s module doc comment for exactly what "top level" excludes and
    // why. Keyed by the same (line, col) an entry's own `UntypedSite` carries;
    // absent for a site this pass didn't attempt (nested origin) or found nothing
    // for. Populated once, after the main statement walk, by
    // `observe_untyped_site_usage`.
    pub(crate) untyped_site_usage: HashMap<(usize, usize), column_usage::ColumnUsage>,
    /// Also work out suggestions while observing usage (see `suggest`). Set by
    /// `suggest_file` and `apply`; off for `check`.
    pub(crate) collect_suggestions: bool,
    /// How `suggest` shows this file's path in a diff header.
    pub(crate) suggest_display_path: String,
    pub(crate) suggestions: Vec<suggest::Suggestion>,
    pub(crate) withheld: Vec<suggest::Withheld>,
    // The `dataframes_typed` counterpart of `untyped_sites` -- every origin that DID
    // resolve to a concrete column set, with its location. Populated at the same
    // choke point (`count_typed_dataframe`'s typed branch) rather than at each of the
    // ~15 individual counting call sites, so this can't drift out of sync with
    // `dataframes_typed` itself.
    pub typed_sites: Vec<TypedSite>,
    // Every `<load module>.<load function>(...)`-shaped call anywhere in the file,
    // found by an unconditional, position-independent scan -- see
    // `LoadCallSiteCollector`. Populated by a single pre-pass in `check_parsed_module`,
    // same timing as `dataframe_shaped_usage`. Deliberately NOT filtered down to only
    // the sites the targeted Assign/AnnAssign counting logic actually recognized: the
    // whole point is to also surface the ones it doesn't (a bare `return`, a call
    // argument, a collection element, ...) so `--coverage-detail=explain` can show
    // them as NOT COUNTED rather than silently omitting them.
    pub all_dataframe_calls: Vec<DataFrameCallSite>,
    // Every statement after which a tracked frame stopped carrying its column set, with
    // the reason -- see `LegEvent`. Recorded at each point a leg is ended, so
    // `--coverage-detail=explain` can answer "why did this frame lose tracking".
    pub leg_events: Vec<LegEvent>,
    // Candidate pass-through-count reversals, func_name -> the origin site to remove
    // once confirmed -- see `PendingPassthroughReversal`'s doc comment and the
    // `Stmt::FunctionDef` body-inference arm that populates this. Resolved at the end
    // of this file's walk (`check_parsed_module`): a candidate whose func_name is in
    // `functions_called` has its origin actually removed; one that never gets
    // confirmed keeps its original count exactly where it was, rather than risking a
    // silent loss.
    pub(crate) pending_passthrough_reversals: HashMap<String, PendingPassthroughReversal>,
    // Every function/method name (the bare name, e.g. "fetch_orders") whose
    // `self.functions`/`class_methods` entry was actually consulted by a resolving
    // call site (a bare `Expr::Name` call or a same-file `self.<attr>.<method>()`)
    // anywhere in THIS file -- the confirmation signal `pending_passthrough_reversals`
    // waits for. Scoped to this file only: a function called solely from another file
    // is never confirmed here (see the reversal site's doc comment for why that's a
    // deliberate, documented gap rather than an oversight).
    pub(crate) functions_called: std::collections::HashSet<String>,
    // Every bare-Name variable subscripted (`x["col"]`) or method-called with a
    // RESERVED_METHODS name (`x.groupby(...)`) anywhere in the module. Populated by a
    // whole-module pre-pass (see DataFrameShapedUsageCollector), same timing as
    // `string_var_candidates` and for the same reason: a name used this way further
    // down the file still needs to be recognized when its assignment is visited
    // earlier. Consulted only when recording `unresolved_dataframe_shaped_calls`.
    pub(crate) dataframe_shaped_usage: std::collections::HashSet<String>,
    // Every `x = call(...)` assignment where `call` didn't match any recognized
    // DataFrame origin pattern, but `x` is later used the way a DataFrame is used
    // (per `dataframe_shaped_usage`). Purely a data-collection side channel — does
    // NOT affect `dataframes_total`, `untyped_sites`, or emit any `LintError` (an
    // earlier version of this did exactly that and had to be reverted: it clashed
    // with an intentionally-silent default and had real false positives, e.g.
    // `logger.info(...)` matching RESERVED_METHODS' `info`). This field exists purely
    // so `index.rs` can discover, from the project's own first-party code, which
    // externally-imported packages are plausible candidates for on-demand external
    // tracing (see `discover_external_package_candidates`) — a lint-time decision
    // never sees this field at all.
    pub(crate) unresolved_dataframe_shaped_calls: Vec<UnresolvedDataframeShapedCall>,
    // Dialect used to fold unquoted SQL identifier case when inferring columns from a
    // literal SELECT list. Defaults to `Generic` (no folding); set from
    // `[tool.typedframes] sql_dialect` in pyproject.toml via `with_context`.
    pub(crate) sql_dialect: sql::SqlDialect,
    // Project root, used to resolve and safety-check `.sql` file reads traced back from
    // a load call (see `read_sql_file`). `None` for standalone/no-config invocations,
    // in which case file-based SQL tracing is skipped entirely rather than guessed at.
    pub(crate) project_root: Option<PathBuf>,
    // Constants and functions of the project modules this file imports, for `static_eval`.
    pub(super) imported_modules: static_eval::ImportedModules,
    // Set once indexing reads a file other than the one being checked (a `.sql` file), so
    // a cached result for this file can't be trusted to track its own contents alone.
    pub(crate) read_external_file: std::cell::Cell<bool>,
    // The frame each unresolved schema leg descends from, and the frames already reported
    // as unverifiable: one `unverifiable-column` per frame, not one per use.
    pub(super) unresolved_root: HashMap<String, String>,
    // Unresolved legs that still know some columns the frame certainly has (what it had before a
    // write under a computed name), so a read of one of those is not reported as unverifiable.
    pub(super) partial_columns: HashMap<String, Vec<String>>,
    // The enclosing `for` loops that run over a fixed list: the names bound and each iteration's values.
    pub(super) loop_bindings: Vec<LoopBinding>,
    // The file imports polars and not pandas, so a method both libraries have (`join`) is
    // read the polars way.
    pub(super) polars_only_file: bool,
    // Names that always hold a polars frame, for the methods both libraries have.
    pub(super) polars_names: std::collections::HashSet<String>,
    // Every column name a frame of this file is known to have, once the walk is done.
    pub(super) known_columns: Vec<String>,
    pub(super) reported_unverifiable: std::cell::RefCell<std::collections::HashSet<String>>,
    // Schema name -> the index levels of a frame that groupby/pivot_table gave a named index,
    // which `reset_index()` turns back into columns.
    pub(super) schema_index: HashMap<String, Vec<String>>,
    // Schemas of frames given an index whose names are not known (`index=idx`,
    // `df.index.name = name`): any argument name may be an index level.
    pub(super) unknown_index: std::collections::HashSet<String>,
    // DataFrames known only by name, in the scope being visited -- see `OpenFrame`.
    pub(super) open_frames: HashMap<String, OpenFrame>,
    // (block depth, name, outer value) of open frames rebound inside a nested block.
    pub(super) shadowed_frames: Vec<(usize, String, OpenFrame)>,
    // How many blocks (branch, loop, `with`, `try`, function) deep the statement being
    // visited is, and which statement of the file it is.
    pub(super) block_depth: usize,
    pub(super) stmt_counter: usize,
    // (frame, column) -> (statement, line, col) of the first read of a column the frame was
    // not known to have, for the read-before-assignment warning.
    pub(super) open_reads: std::cell::RefCell<HashMap<(String, String), FirstRead>>,
    // (line, col, column) of every literal column read off a frame whose columns are unknown,
    // for `likely-column-typo`.
    pub(super) unknown_frame_reads: std::cell::RefCell<Vec<(usize, usize, String)>>,
    // Calls already checked against a callee's parameter contract, by (file, offset): the
    // statement-level check and the nested-call walk both reach a call written in place.
    pub(super) contract_calls_checked: std::cell::RefCell<std::collections::HashSet<(String, u32)>>,
    // Column requirements per parameter (see `call_contracts`), keyed like helper summaries:
    // `(None, f)` for this file's functions, `(Some(Class), m)` for methods, and
    // `(Some(IMPORTED_SLOT), local)` / `(Some("module:alias"), f)` for imported ones.
    pub(super) contracts: HashMap<FnKey, (call_contracts::FuncContract, String)>,
    // This file's own contracts, with forwards only the project index can resolve.
    pub(crate) local_contracts: HashMap<FnKey, call_contracts::FuncContract>,
    pub(crate) imported_contracts: HashMap<FnKey, (call_contracts::FuncContract, String)>,
    // Callee position -> contract key, for calls whose name binds to one function or module.
    pub(crate) contract_targets: HashMap<u32, FnKey>,
    // Set when `contract_targets` was already built from the module as written.
    pub(crate) contract_targets_ready: bool,
    // Methods each class of this file defines, and which classes are protocols.
    pub(crate) class_method_names: HashMap<String, std::collections::HashSet<String>>,
    pub(crate) protocol_classes: std::collections::HashSet<String>,
    // Class -> the classes of this file that have it among their ancestors.
    pub(super) subclasses: HashMap<String, Vec<String>>,
    // Class -> methods a class of another file overrides on it (see `ProjectIndex::overridden`).
    pub(super) overridden_elsewhere: HashMap<String, std::collections::HashSet<String>>,
    // Requirement every implementor in the project agrees on, by method name.
    pub(super) method_agreement: HashMap<String, (call_contracts::FuncContract, String)>,
    pub(super) method_contract_memo: std::cell::RefCell<HashMap<FnKey, Option<FnKey>>>,
    // Undefined members of string enums, found before folding replaced the defined ones.
    pub(super) missing_enum_members: Vec<static_eval::MissingMember>,
    // The value of the assignment being visited was already checked, against the frames as
    // they were before the statement rebound one of them.
    pub(super) value_checked_early: bool,
    // Libraries of functions imported from other project files, by name, from the index.
    pub(super) imported_function_libraries: HashMap<String, suggest::Library>,
    // Libraries of the methods of classes imported from other project files, by
    // `(class, method)`, from the index.
    pub(super) imported_class_method_libraries: HashMap<(String, String), suggest::Library>,
    // What functions imported from other project files do with their parameters, by local
    // name, from the index.
    pub(super) imported_param_usage: HashMap<String, Vec<column_usage::ParamUsage>>,
    // The project index this file is checked against, for what every caller of a function
    // does with the frame it returns (`ProjectIndex::call_usage`).
    pub(crate) project_index: Option<Arc<ProjectIndex>>,
    // Names bound to a plain string literal (or a resolvable `.sql`/text file read —
    // see `resolve_literal_rhs`) exactly once anywhere in the module. Populated by a
    // `StringBindingCollector` pre-pass in `check_file_internal`, before the main
    // statement walk, so that e.g. a module-level `QUERY = "..."` constant is visible
    // no matter where in the file it's used. A name reassigned, conditionally assigned,
    // augmented-assigned, or bound by anything other than a plain literal/file-read is
    // absent from this map entirely — see `StringBindingCollector::record` for the
    // poison-on-any-second-binding policy this relies on.
    pub(crate) string_var_candidates: HashMap<String, String>,
    // Names bound exactly once in the whole file, to a `True`/`False` literal -- the
    // boolean counterpart of `string_var_candidates`, for `inplace=flag`.
    pub(crate) bool_var_candidates: HashMap<String, bool>,
    // Names initialized as `name = []` and thereafter only ever added to via
    // `name.append({...})` with a literal dict of string-literal keys, right up to
    // being read (as the first positional argument) by a `DataFrame(name)` call --
    // the accumulator counterpart of `pd.DataFrame([{...}, {...}])`. Maps to the
    // union of every append's keys, in first-seen order. Also holds a name bound
    // exactly once to a dict literal or a literal list of dicts, which stands for the
    // same thing once passed to `DataFrame(name)`. Populated by a
    // `DictListBindingCollector` pre-pass alongside `string_var_candidates`; poisoned
    // (absent from this map) by a second `= []`, any other reassignment, any
    // `.append()` call whose argument isn't such a dict literal, or any other bare
    // reference to the name at all (passed to a function, checked for truthiness,
    // indexed, ...) -- same "give up rather than guess" policy as
    // `string_var_candidates`, deliberately conservative since a missed mutation here
    // would mean inferring a schema real code could add columns past.
    pub(crate) dict_list_var_candidates: HashMap<String, Vec<String>>,
    // Names bound to a resolved SQLAlchemy Core `select(...)` column list — e.g.
    // `stmt = select(Order.id, Order.amount)`. Populated inline during the main
    // top-to-bottom statement walk (unlike `string_var_candidates`, which needs a
    // whole-module pre-pass): resolving a `select(...)` call requires the referenced
    // model's columns to already be in `self.schemas`, which — like every other
    // variable/schema binding this checker tracks — is only guaranteed for a class
    // defined earlier in the same file (or in another file, via the project index).
    // A later reassignment simply overwrites the entry, consistent with how
    // `self.variables` already behaves elsewhere in this checker.
    pub(crate) stmt_var_candidates: HashMap<String, Vec<String>>,
    // Names bound to a Feast retrieval's resolved feature columns, plus entity_df's
    // own columns when those resolved too (see `resolve_feast_entity_columns`) --
    // BEFORE `.to_df()` (the split form: `job = store.get_...(...)`, then
    // `df = job.to_df()`). Kept separate from `self.variables`/`self.schemas`: a
    // RetrievalJob/OnlineResponse isn't a DataFrame, so `job["x"]` shouldn't be
    // validated as a column access. See `register_feast_dataframe`.
    pub(crate) retrieval_jobs: HashMap<String, (Vec<String>, Option<Vec<String>>)>,
    // Schema names for a recognized DataFrame origin with no resolvable column
    // list: a Feast retrieval whose entity_df isn't concretely typed (see
    // `register_feast_dataframe` -- a typed entity_df gets unioned into a real
    // schema instead and never lands here), or a bare `-> pd.DataFrame`/
    // `-> pl.DataFrame` return with no attached Schema. Every access against one
    // of these raises `unverifiable-column` (see `schema_has_column`'s call sites)
    // rather than passing or false-positiving on a column this checker can't see.
    pub(crate) unresolved_schemas: std::collections::HashSet<String>,
    // Functions that call one of their own parameters with their `*args`, as `fn(*args)`:
    // name -> (the callable's position, the number of positional parameters before `*args`).
    pub(super) forwarders: HashMap<String, (usize, usize)>,
    // Cursor variable name -> the SQL text most recently passed to `cursor.execute(sql)`
    // (the PEP 249 pattern used by Snowflake, Redshift, and similar connectors), until
    // a later `cursor.fetch_pandas_all()` materializes it into a DataFrame. A second
    // `execute()` on the same cursor overwrites (or, if its argument doesn't resolve,
    // removes) the previous entry — matching real PEP 249 semantics, where a cursor
    // holds exactly one most-recently-executed query at a time.
    pub(crate) cursor_sql: HashMap<String, String>,
    // func_name -> a recognized "parameter feeds a Feast features= call, whose result is
    // subscripted in the same body" shape, found while indexing this file's functions.
    // Consumed once, project-wide, by resolve_param_governed_call_sites -- see
    // ParamGovernedTemplate's doc comment.
    pub(crate) param_governed_templates: HashMap<String, ParamGovernedTemplate>,
    // Every top-level function name defined anywhere in this file, regardless of
    // whether it has any schema/requires/param-governed info of its own -- needed so
    // resolve_delegate_target can find a plain helper (e.g. one that just returns a
    // literal list or f-string) as a valid call target for eval_feast_call, which
    // re-parses the target file itself rather than needing anything precomputed here.
    pub(crate) all_function_names: std::collections::BTreeSet<String>,
    // Every function/method name defined ANYWHERE in this file, collected by a
    // pre-pass -- unlike `all_function_names`, which only grows as the walk reaches each
    // definition. Lets a reassignment from a call to a helper defined further down the
    // file be told apart from a call to a genuinely unknown function: the walk simply
    // hasn't inferred that helper's return schema yet.
    pub(crate) file_function_names: std::collections::HashSet<String>,
    // What a call does to the frame it is handed, per function (or method) defined in
    // this file -- see `frame_ops::summarize_helper`. Keyed by defining class (`None` for a
    // plain function), so a `self.m()` call only ever meets its own class's `m`.
    pub(crate) helper_summaries: HashMap<FnKey, HelperSummary>,
    // Set only by `index::index_file`: a call to a name this file does not itself define
    // may still be a cross-file helper the project index hasn't resolved yet, so
    // `resolve_delegates` keeps the raw reference for `resolve_transitive_helper_summaries`
    // to finish, rather than dropping it as a confirmed no-op. Never set for a real check
    // (`check_file`/`check_notebook`), where every resolvable name is already merged into
    // `imported_summaries` beforehand, so "not found" there does mean no effect.
    pub(crate) preserve_unresolved_delegates: bool,
    // Summaries of functions imported from other files, keyed like `helper_summaries`: a
    // plain function under `None`, and one reached as `module.f(...)` under the class slot
    // `module:<alias>`. Seeded by `load_cross_file_symbols`; a local definition wins.
    pub(crate) imported_summaries: HashMap<FnKey, HelperSummary>,
    // Names of the methods defined directly in the class currently being visited, so a
    // `self.<method>()` call resolves against this class rather than any same-named
    // function elsewhere in the flat `functions` map.
    pub(crate) current_class_methods: std::collections::HashSet<String>,
    // Return schema (or `OPEN_FRAME_MARKER`) of each method of the current class that
    // has been visited so far, keyed by method name -- `self.functions` is one flat map
    // across classes and modules, so `self.<method>()` reads this instead.
    pub(crate) current_class_functions: HashMap<String, String>,
    pub(crate) current_class_name: Option<String>,
    // Names known to hold an instance of a class defined in this file -- parameters
    // annotated with it (`source: DataSource`) and variables assigned from its constructor
    // (`loader = Loader()`) -- so `source.load()` can be resolved against that class's
    // methods. Per function; a nested function starts from its enclosing function's.
    pub(crate) receiver_classes: HashMap<String, String>,
    // Pydantic models and dataclasses defined in this file, by name, with their field names;
    // and the parameters of the function being visited annotated with one, so
    // `pd.DataFrame([txn.model_dump()])` has the model's fields as its columns.
    pub(super) record_fields: HashMap<String, Vec<String>>,
    pub(super) record_params: HashMap<String, String>,
    // Instance attribute name -> class name, for `self.<attr> = <ClassName>(...)` /
    // `self.<attr> = <module>.<ClassName>(...)` assignments found in the CURRENTLY
    // VISITED class's own `__init__`. Populated by a pre-scan in the `Stmt::ClassDef`
    // arm, before `visit_body` descends into the rest of that class's methods, and
    // saved/restored around that call so a nested class (rare, but not impossible)
    // can't leak its own attrs into (or lose) an enclosing class's map. This is what
    // makes `self._data_repository.get(...)` resolvable at all: without knowing what
    // `_data_repository` IS, there's no receiver to look anything up against, the same
    // way `self.functions`/class_methods below need a class name to key on. Single-
    // assignment only, `__init__`-only -- an attribute reassigned to a different type
    // later, or set up in some other method, is intentionally not tracked (matches
    // this checker's existing single-binding conventions, e.g. `string_var_candidates`).
    pub(crate) current_self_attrs: HashMap<String, SelfAttrOrigin>,
    // Every self.<attr> origin ever seen anywhere in this file, across every class --
    // unlike `current_self_attrs`, never reset per-class. Consulted only by
    // `index.rs::index_file`, once this file's whole linter pass is done, to find
    // `py.typed`-declared external packages worth tracing immediately (see
    // `package_declares_py_typed`) -- a py.typed declaration is itself a strong enough
    // trust signal that this doesn't need to wait for `dataframe_shaped_usage`'s
    // "was the result actually used like a DataFrame" confirmation the way the plain
    // `unresolved_dataframe_shaped_calls` discovery path does. A rare same-named attr
    // on two different classes in one file can collide here (last one wins) -- harmless
    // for this purpose, since it's only ever used to gather auto-discovery CANDIDATES,
    // never to resolve an actual column set (that precision lives in
    // `current_self_attrs`/`class_methods`, both unaffected by this field).
    pub(crate) all_self_attr_origins: HashMap<String, SelfAttrOrigin>,
    // Frames stored on `self`, by the start of each method they are rewritten in.
    pub(super) method_frames: crate::self_frames::MethodFrames,
    // Class name -> {method name -> return schema name}, for every class VISITED SO
    // FAR in this file (top-to-bottom, like every other map this checker builds while
    // walking a file -- a class defined textually AFTER its use isn't in here yet,
    // same single-pass ordering limitation already documented for contract inference
    // elsewhere in this file). The schema value follows the same convention as
    // `self.functions`: a real name, or OPEN_FRAME_MARKER for a bare
    // `pd.DataFrame`/`pl.DataFrame` return with no attached Schema (see its doc
    // comment), registered with an open/empty schema the same way
    // `register_feast_dataframe` already does for Feast. This is what lets
    // `self.<attr>.<method>(...)` resolve to a schema once `<attr>`'s class (from
    // `current_self_attrs`) is known, FOR A DIFFERENT CLASS than the one currently
    // being visited -- unlike `current_self_attrs`, this is never reset: a class's
    // methods, once seen, stay resolvable for the rest of the file, the same way
    // `self.schemas`/`self.functions` accumulate. A class defined in another file
    // needs the project index's own `class_methods` (see index.rs) instead.
    pub(crate) class_methods: HashMap<String, HashMap<String, String>>,
    // Base-class names of every class defined in this file, so an inherited method can be
    // found. Populated before the walk, so a class may inherit from one defined later.
    pub(crate) class_bases: HashMap<String, Vec<String>>,
}

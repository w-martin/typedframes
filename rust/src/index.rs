//! Cross-file project index: walks the project's `.py` files, extracts a
//! lightweight symbol table, and resolves transitive column requirements
//! across function calls.
//!
//! Mutually dependent with [`crate::Linter`]: indexing reuses the linter to scan
//! individual files, and the linter calls back here to resolve cross-file symbols.

use crate::ast_extract;
use crate::config::{find_project_root_opt, load_linter_config, LinterConfig};
use crate::constants::DEFAULT_EXCLUDED_DIRS;
use crate::errors::{LintError, CODE_UNKNOWN_COLUMN, CODE_UNTRACKED_DATAFRAME};
use crate::linter::{Linter, ParamGovernedTemplate};
use ruff_python_ast::{self as ast, Expr, Stmt};
use ruff_python_parser::parse_module;
use ruff_source_file::{LineIndex, SourceCode};
use ruff_text_size::Ranged;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

// ── Index structs ──────────────────────────────────────────────────────────────

// Return-type and parameter-contract information extracted from a function definition.
#[derive(Serialize, Deserialize)]
pub(crate) struct IndexFunction {
    pub(crate) returns_schema: String, // BaseSchema subclass name, e.g. "OrderSchema"; empty if none
    pub(crate) returns_frame: String,  // reserved for future use
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
}

// Symbol table for a single `.py` file, stored inside ProjectIndex.
#[derive(Serialize, Deserialize)]
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
}

// In-memory cross-file symbol index.
// Serialised as MessagePack so it can be held in Python memory and passed to check_file
// without any intermediate disk I/O.  The version field allows future format migrations.
#[derive(Serialize, Deserialize)]
pub(crate) struct ProjectIndex {
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
}

// Union schema name -> column list across every file's `schemas` map. First
// definition wins on name collision (matches the prior per-call behaviour in both
// resolve_param_schema_requires and load_cross_file_symbols).
pub(crate) fn compute_all_schemas(
    files: &HashMap<String, IndexEntry>,
) -> HashMap<String, Vec<String>> {
    let mut all_schemas: HashMap<String, Vec<String>> = HashMap::new();
    for entry in files.values() {
        for (name, cols) in &entry.schemas {
            all_schemas
                .entry(name.clone())
                .or_insert_with(|| cols.clone());
        }
    }
    all_schemas
}

// Union schema name -> (file, definition line) across every file's `schema_locations`
// map, mirroring compute_all_schemas -- a schema's class may live in a THIRD file,
// neither the function's own file nor the one importing it, same reasoning as
// `all_schemas` itself.
pub(crate) fn compute_all_schema_locations(
    files: &HashMap<String, IndexEntry>,
) -> HashMap<String, (String, usize)> {
    let mut all_schema_locations: HashMap<String, (String, usize)> = HashMap::new();
    for entry in files.values() {
        for (name, loc) in &entry.schema_locations {
            all_schema_locations
                .entry(name.clone())
                .or_insert_with(|| loc.clone());
        }
    }
    all_schema_locations
}

// ── Index helpers ──────────────────────────────────────────────────────────────

// Recursively collect all `.py` files under `dir`, skipping any name in
// `resolved_excludes` -- the caller's job to resolve (see `build_index_internal`):
// either `[tool.typedframes] exclude`'s configured list, replacing
// `DEFAULT_EXCLUDED_DIRS` entirely, or `DEFAULT_EXCLUDED_DIRS` itself when nothing is
// configured. Uses an explicit stack rather than recursion to avoid stack overflow on
// very deep trees.
pub(crate) fn collect_py_files(
    dir: &Path,
    resolved_excludes: &std::collections::HashSet<String>,
) -> Vec<PathBuf> {
    let mut result = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if resolved_excludes.contains(name_str.as_ref()) {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("py") {
                result.push(path);
            }
        }
    }
    result
}

// Auto-detect a project's virtualenv site-packages directory, for resolving
// explicitly allowlisted external packages (`trace_external_packages` config option).
// Tries the conventional `.venv` layout on both Unix (`lib/pythonX.Y/site-packages`,
// version-globbed since it depends on the interpreter the venv was created with) and
// Windows (`Lib/site-packages`). No path override in this version — this is the
// convention this project's own examples use (a `uv`-managed `.venv`); a project using
// a differently-named or externally-managed environment simply won't resolve anything,
// same as any other candidate path that doesn't exist.
// Memoized because `resolve_module_file` — which calls this — runs once per import
// statement across an entire project check, and the answer can never change within a
// single process's lifetime (see INDEX_CACHE above for the same reasoning). Without
// this, a large project with a real `.venv` would re-`read_dir` the same `lib/`
// directory on every single import resolution, turning one cheap directory listing
// into one per import statement in the whole project.
pub(crate) static SITE_PACKAGES_CACHE: Mutex<Option<(PathBuf, Option<PathBuf>)>> = Mutex::new(None);

pub(crate) fn find_site_packages_dir(project_root: &Path) -> Option<PathBuf> {
    if let Ok(cache) = SITE_PACKAGES_CACHE.lock() {
        if let Some((cached_root, result)) = cache.as_ref() {
            if cached_root == project_root {
                return result.clone();
            }
        }
    }
    let result = find_site_packages_dir_uncached(project_root);
    if let Ok(mut cache) = SITE_PACKAGES_CACHE.lock() {
        *cache = Some((project_root.to_path_buf(), result.clone()));
    }
    result
}

pub(crate) fn find_site_packages_dir_uncached(project_root: &Path) -> Option<PathBuf> {
    let venv = project_root.join(".venv");
    if !venv.is_dir() {
        return None;
    }
    let windows_layout = venv.join("Lib").join("site-packages");
    if windows_layout.is_dir() {
        return Some(windows_layout);
    }
    let lib_dir = venv.join("lib");
    let entries = fs::read_dir(&lib_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if !path
            .file_name()
            .map(|n| n.to_string_lossy().starts_with("python"))
            .unwrap_or(false)
        {
            continue;
        }
        let candidate = path.join("site-packages");
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    None
}

// Whether an external package declares PEP 561 `py.typed` -- a plain marker file at
// the package's own root (`<site-packages>/<package>/py.typed`), meaning its author
// intends its inline (or bundled `.pyi`) type annotations to be trusted by tooling.
// Used to let a self.<attr>.<method>(...) call resolve immediately against that
// package's actual return-type annotations (once traced -- see index_file's
// `all_self_attr_origins` handling below), rather than needing
// `dataframe_shaped_usage`'s "was the result actually used like a DataFrame"
// confirmation first, the way plain auto-discovery still does for packages that don't
// make this promise. Absence isn't a judgment on the package -- it just means this
// checker has no first-party signal that its annotations (if any) are meant to be
// consumed, so it falls back to behavioral discovery like everything else.
pub(crate) fn package_declares_py_typed(project_root: &Path, package: &str) -> bool {
    let Some(site_packages) = find_site_packages_dir(project_root) else {
        return false;
    };
    site_packages.join(package).join("py.typed").is_file()
}

// Collect `.py` files for each named external package, resolved from the project's
// own site-packages directory. `packages` is the already-merged, already-filtered set
// -- see `resolve_traced_external_packages` for how `trace_external_packages`
// (explicit force-include), auto-discovered candidates, and `excluded_external_packages`
// combine into it. Deliberately narrow: only the named packages' own directories are
// walked — never the whole site-packages tree, which would be both expensive and a far
// larger, unbounded trust surface than a bounded, explainable package list.
pub(crate) fn collect_external_package_files(
    project_root: &Path,
    packages: &[String],
) -> Vec<PathBuf> {
    if packages.is_empty() {
        return Vec::new();
    }
    let Some(site_packages) = find_site_packages_dir(project_root) else {
        return Vec::new();
    };
    let mut result = Vec::new();
    for package in packages {
        let package_dir = site_packages.join(package);
        if package_dir.is_dir() {
            // Never excludes anything here -- `exclude` is about pruning irrelevant
            // project-local directories, not the internal structure of a package the
            // user has explicitly opted into tracing.
            result.extend(collect_py_files(
                &package_dir,
                &std::collections::HashSet::new(),
            ));
            continue;
        }
        let single_module = site_packages.join(format!("{package}.py"));
        if single_module.is_file() {
            result.push(single_module);
        }
    }
    result
}

// Parse one `.py` file and extract its symbols into an IndexEntry.
// Runs the linter in index mode (diagnostics discarded) to collect schemas and
// functions, then separately parses `__all__` assignments and `from X import Y`
// statements for wildcard-import support and delegate-target resolution respectively.
// Returns the file's `IndexEntry` plus the top-level package names of any external
// (non-first-party) imports it called in an unresolved, DataFrame-shaped way (see
// `Linter::unresolved_dataframe_shaped_calls`) -- candidates for on-demand external
// package tracing, resolved by `discover_external_package_candidates` in
// `build_index_internal`. This file's own imports/module_aliases are exactly what's
// needed to resolve each candidate's raw call-site name to a module, so it's done
// here rather than requiring a second pass over the same file.
pub(crate) fn index_file(
    path: &Path,
    project_root: &Path,
    config: &LinterConfig,
) -> Option<(IndexEntry, Vec<String>)> {
    let source = fs::read_to_string(path).ok()?;

    let mut linter = Linter::new();
    linter.with_context(project_root.to_path_buf(), config);
    let _ = linter.check_file_internal(&source, path);

    let schemas = linter.schemas;
    let schema_locations = linter.schema_locations;
    // Union function names across the return-schema, requires, delegates, and
    // param-schema-name maps — a function may appear in any subset of the four.
    let mut func_names: std::collections::BTreeSet<String> =
        linter.functions.keys().cloned().collect();
    func_names.extend(linter.requires.keys().cloned());
    func_names.extend(linter.delegates.keys().cloned());
    func_names.extend(linter.param_schema_names.keys().cloned());
    func_names.extend(linter.param_governed_templates.keys().cloned());
    func_names.extend(linter.all_function_names.iter().cloned());
    let functions: HashMap<String, IndexFunction> = func_names
        .into_iter()
        .map(|name| {
            let returns_schema = linter.functions.get(&name).cloned().unwrap_or_default();
            let (requires, def_line) = linter
                .requires
                .get(&name)
                .cloned()
                .unwrap_or((Vec::new(), 0));
            let delegates = linter.delegates.get(&name).cloned().unwrap_or_default();
            let (param_schema_name, param_def_line) = linter
                .param_schema_names
                .get(&name)
                .cloned()
                .unwrap_or_default();
            let param_governed = linter.param_governed_templates.get(&name).cloned();
            // def_line may only be known via param_schema_names if this function had
            // no direct requires/delegates of its own (see the gate in visit_stmt).
            let def_line = if def_line == 0 {
                param_def_line
            } else {
                def_line
            };
            (
                name,
                IndexFunction {
                    returns_schema,
                    returns_frame: String::new(),
                    requires,
                    def_line,
                    delegates,
                    param_schema_name,
                    param_governed,
                },
            )
        })
        .collect();

    let mut exports = Vec::new();
    let mut imports: HashMap<String, String> = HashMap::new();
    let mut module_aliases: HashMap<String, String> = HashMap::new();
    if let Ok(parsed) = parse_module(&source) {
        let module = parsed.into_syntax();
        for stmt in &module.body {
            match stmt {
                Stmt::Assign(assign) => {
                    for target in &assign.targets {
                        let Expr::Name(name) = target else {
                            continue;
                        };
                        if name.id.as_str() != "__all__" {
                            continue;
                        }
                        let Expr::List(list) = &*assign.value else {
                            continue;
                        };
                        for el in &list.elts {
                            if let Expr::StringLiteral(s) = el {
                                exports.push(s.value.to_str().to_string());
                            }
                        }
                    }
                }
                Stmt::ImportFrom(import_from) if import_from.level == 0 => {
                    if let Some(module_ident) = &import_from.module {
                        let module_name = module_ident.id.to_string();
                        for alias in &import_from.names {
                            imports.insert(alias.name.id.to_string(), module_name.clone());
                        }
                    }
                }
                // Plain `import module [as alias]` / `import a.b.c [as alias]`. Without
                // an alias, Python binds the FIRST segment of a dotted import (`import
                // a.b.c` binds `a`), so a dotted single-segment module (the common case
                // for a flat project: `import transforms`) binds cleanly; a genuinely
                // nested `a.b.c` accessed as `a.b.c.func()` without an alias is not
                // tracked here (`a.func()` wouldn't resolve to `a.b.c` anyway) — same
                // conservative-not-exhaustive trade-off as the rest of this heuristic.
                Stmt::Import(import_stmt) => {
                    for alias in &import_stmt.names {
                        let dotted = alias.name.id.to_string();
                        let local_name = match &alias.asname {
                            Some(asname) => asname.id.to_string(),
                            None => dotted.split('.').next().unwrap_or(&dotted).to_string(),
                        };
                        module_aliases.insert(local_name, dotted);
                    }
                }
                _ => {}
            }
        }
    }

    // Resolve a recorded unresolved-call name (an attribute-call receiver or a
    // bare-call callee) to the module it was imported from, then take that module's
    // top-level package name -- matching the same dotted-import-binds-first-segment
    // convention as the `Stmt::Import` handling above. A name that isn't a plain
    // import at all (a local variable, a class instance, a closure) simply has no
    // entry in `imports`/`module_aliases` and is silently dropped here, same as any
    // other unresolvable name elsewhere in this module.
    let resolve_top_level_package = |name: &str, is_attribute_call: bool| -> Option<String> {
        let module_name = if is_attribute_call {
            module_aliases.get(name)
        } else {
            imports.get(name)
        }?;
        module_name.split('.').next().map(str::to_string)
    };

    let mut external_package_candidates: Vec<String> = linter
        .unresolved_dataframe_shaped_calls
        .iter()
        .filter_map(|call| resolve_top_level_package(&call.name, call.is_attribute_call))
        .collect();

    // self.<attr>.<method>(...) receivers whose class resolves to a py.typed-declared
    // package -- traced immediately, without needing dataframe_shaped_usage's "was
    // the result actually used like a DataFrame" confirmation the way the list above
    // requires (see `package_declares_py_typed`'s doc comment for why a py.typed
    // declaration alone is trust enough). Duplicates against the list above collapse
    // naturally once both feed the same `discovered_candidates` HashSet in
    // `build_index_internal`.
    for origin in linter.all_self_attr_origins.values() {
        if let Some(package) =
            resolve_top_level_package(&origin.resolve_name, origin.is_attribute_call)
        {
            if package_declares_py_typed(project_root, &package) {
                external_package_candidates.push(package);
            }
        }
    }

    Some((
        IndexEntry {
            schemas,
            schema_locations,
            functions,
            exports,
            imports,
            module_aliases,
            class_methods: linter.class_methods,
        },
        external_package_candidates,
    ))
}

// Ceiling on how many AUTO-discovered candidate packages (see
// `discover_external_package_candidates`) get traced by default. Never applies to
// `trace_external_packages` -- an explicit force-include is never capped, since the
// user opted into it deliberately and by name. Bounds the worst case for a project
// whose first-party code happens to call many different external packages in
// DataFrame-shaped ways, in the same spirit as the MAX_SQL_FILE_READS_PER_FILE /
// MAX_SQL_FILE_BYTES guardrails in linter.rs's own `read_sql_file`.
pub(crate) const MAX_AUTO_TRACED_PACKAGES: usize = 20;

// Merge `trace_external_packages` (forced, uncapped, exempt from
// `excluded_external_packages`), auto-discovered candidates (capped at
// MAX_AUTO_TRACED_PACKAGES, alphabetically for determinism, filtered by
// `excluded_external_packages` and by not already being a first-party module), into
// the final list of package names to trace.
pub(crate) fn resolve_traced_external_packages(
    config: &LinterConfig,
    discovered_candidates: std::collections::HashSet<String>,
    project_root: &Path,
    first_party_files: &HashMap<String, IndexEntry>,
) -> Vec<String> {
    let forced: std::collections::HashSet<String> = config
        .trace_external_packages
        .clone()
        .unwrap_or_default()
        .into_iter()
        .collect();
    let excluded: std::collections::HashSet<String> = config
        .excluded_external_packages
        .clone()
        .unwrap_or_default()
        .into_iter()
        .collect();

    let mut auto_candidates: Vec<String> = discovered_candidates
        .into_iter()
        .filter(|name| !forced.contains(name))
        .filter(|name| !excluded.contains(name))
        // A candidate that resolves to a first-party module isn't external at all --
        // e.g. a plain `import utils` inside a `src/` layout project can look exactly
        // like an unresolved external call before cross-file symbols are loaded.
        .filter(|name| resolve_module_file(name, project_root, first_party_files).is_none())
        .collect();
    auto_candidates.sort();
    auto_candidates.truncate(MAX_AUTO_TRACED_PACKAGES);

    let mut result: Vec<String> = forced.into_iter().collect();
    result.extend(auto_candidates);
    result.sort();
    result.dedup();
    result
}

// Build a ProjectIndex by indexing every `.py` file under `project_root`. Two-phase:
// first-party files are indexed on their own (collecting external-package candidates
// along the way, see `index_file`), THEN the resolved external-package list is indexed
// and merged in — see `resolve_traced_external_packages`. This ordering is required,
// not just convenient: deciding which external packages are worth tracing depends on
// having already seen every first-party file's own imports and DataFrame-shaped usage.
pub(crate) fn build_index_internal(project_root: &Path) -> ProjectIndex {
    // Loaded once and threaded into every per-file `Linter`, so index-time inference
    // (this function) and check-time inference (`check_file`) agree on `sql_dialect` —
    // a mismatch here would make a SQL-derived schema's columns differ depending on
    // whether they were read from the cached project index or inferred fresh.
    let config = load_linter_config(project_root);
    let py_files = collect_py_files(project_root, &resolve_excluded_dirs(&config));

    let mut files = HashMap::new();
    let mut discovered_candidates: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for file_path in &py_files {
        if let Some((entry, candidates)) = index_file(file_path, project_root, &config) {
            discovered_candidates.extend(candidates);
            if let Some(path_str) = file_path.to_str() {
                files.insert(path_str.to_string(), entry);
            }
        }
    }

    index_traced_external_packages(project_root, &config, discovered_candidates, &mut files);
    finalise_index(project_root, files)
}

// A configured `exclude` REPLACES DEFAULT_EXCLUDED_DIRS entirely -- it does not add to
// it (see collect_py_files's doc comment).
pub(crate) fn resolve_excluded_dirs(config: &LinterConfig) -> std::collections::HashSet<String> {
    match &config.exclude {
        Some(list) => list.iter().cloned().collect(),
        None => DEFAULT_EXCLUDED_DIRS
            .iter()
            .map(|s| s.to_string())
            .collect(),
    }
}

// Second indexing phase, shared by the whole-project and single-file entry points:
// resolve which external installed packages are worth tracing (see
// `resolve_traced_external_packages` -- pure decision logic over an already-collected
// candidate set, so it works identically whether those candidates came from every file
// in the project or from just one file's own import/call graph), then index their files
// into the same map.
pub(crate) fn index_traced_external_packages(
    project_root: &Path,
    config: &LinterConfig,
    discovered_candidates: std::collections::HashSet<String>,
    files: &mut HashMap<String, IndexEntry>,
) {
    let traced_packages =
        resolve_traced_external_packages(config, discovered_candidates, project_root, files);
    for file_path in collect_external_package_files(project_root, &traced_packages) {
        if let Some((entry, _candidates)) = index_file(&file_path, project_root, config) {
            if let Some(path_str) = file_path.to_str() {
                files.insert(path_str.to_string(), entry);
            }
        }
    }
}

// Final resolution pass, shared by the whole-project and single-file entry points.
// Every step here is a fold over the `files` map that was handed in — it never goes
// looking for more files — so it costs whatever that map contains and nothing more,
// which is exactly what lets single-file mode reuse it unchanged.
pub(crate) fn finalise_index(
    project_root: &Path,
    mut files: HashMap<String, IndexEntry>,
) -> ProjectIndex {
    let all_schemas = compute_all_schemas(&files);
    let all_schema_locations = compute_all_schema_locations(&files);
    resolve_param_schema_requires(&mut files, &all_schemas);
    resolve_transitive_requires(project_root, &mut files);
    let governed = resolve_param_governed_call_sites(project_root, &files);
    ProjectIndex {
        version: 1,
        files,
        all_schemas,
        all_schema_locations,
        call_site_errors: governed.call_site_errors,
        resolved_governed: governed.resolved_governed,
    }
}

// How many import hops past the checked file `build_single_file_index_internal` will
// follow. Depth 1 is the file's own direct imports; depth 2 is what those files
// themselves import, and so on.
//
// Three is the smallest bound that covers the shapes this checker actually needs, with
// one hop of slack:
//   - depth 1: `pipeline.py` imports a schema straight from `schemas.py`.
//   - depth 2: `pipeline.py` imports `load_users` from `loaders.py`, whose return
//     annotation names a schema that lives in a THIRD file, `schemas.py` — the case
//     `all_schemas` exists for (see `compute_all_schemas`), and by far the most common
//     real layout.
//   - depth 3: one more hop for a delegate chain (`resolve_transitive_requires`), where
//     the imported function forwards its DataFrame on to a function in a further file
//     that is the one actually accessing columns.
// Past that, the marginal chance of the extra file mattering to THIS file's diagnostics
// drops off sharply while the fan-out keeps multiplying, so the bound is a deliberate
// "conservative, no surprising slowness" cut-off rather than a technical limit — the
// whole point of single-file mode is staying proportional to what the checked file
// actually references. A project that wants unbounded resolution already has it:
// checking the directory.
pub(crate) const MAX_SINGLE_FILE_IMPORT_DEPTH: usize = 3;

// Hard ceiling on how many first-party files the single-file import walk will index,
// regardless of depth. Bounds the pathological case the depth limit alone doesn't: a
// file importing a wide module which itself imports another wide module, where each
// hop multiplies rather than adds. 200 is comfortably above any realistic import
// closure at depth 3 (the shapes above reach 2-5 files) while still being a small
// fraction of a large project's file count, so hitting it means the walk had already
// stopped resembling "this file's own references" — the same spirit as
// MAX_AUTO_TRACED_PACKAGES here and MAX_SQL_FILE_READS_PER_FILE in linter.rs.
pub(crate) const MAX_SINGLE_FILE_INDEXED_FILES: usize = 200;

// Resolve a dotted module name to a first-party file ON DISK, without needing an
// already-populated index the way `resolve_module_file` does. Used only by the
// single-file import walk, which is discovering the files to index in the first place
// and so has nothing to look them up in yet.
//
// Deliberately tries exactly the same two roots `resolve_module_file` does — the
// project root and a `src/` layout — so that every file this walk decides to index is
// also a file check-time resolution (`Linter::load_cross_file_symbols`) can actually
// find again. A package `__init__.py` is NOT a candidate for the same reason: check
// time wouldn't resolve `from pkg import X` to it either (in whole-project mode just
// as much as here), so indexing it would add cost for a symbol nothing would ever look
// up. A resolved path under an excluded directory is dropped, matching what
// `collect_py_files` would have pruned in whole-project mode.
pub(crate) fn resolve_first_party_module_path(
    module_name: &str,
    project_root: &Path,
    excluded_dirs: &std::collections::HashSet<String>,
) -> Option<PathBuf> {
    let mod_path = module_name.replace('.', "/");
    let candidate = [
        project_root.join(format!("{mod_path}.py")),
        project_root.join("src").join(format!("{mod_path}.py")),
    ]
    .into_iter()
    .find(|p| p.is_file())?;
    let under_excluded_dir = candidate
        .strip_prefix(project_root)
        .ok()?
        .components()
        .any(|c| excluded_dirs.contains(c.as_os_str().to_string_lossy().as_ref()));
    (!under_excluded_dir).then_some(candidate)
}

// Build a ProjectIndex scoped to ONE file: the file itself, the project-local modules
// its own `import` / `from ... import ...` statements reach (transitively, bounded by
// MAX_SINGLE_FILE_IMPORT_DEPTH and MAX_SINGLE_FILE_INDEXED_FILES), and any external
// installed package that closure calls in a DataFrame-shaped way.
//
// This is the single-file counterpart to `build_index_internal`, and the difference is
// entirely in how the file set is chosen: whole-project mode walks every `.py` file
// under the root (`collect_py_files`) because it has to produce an index valid for
// every file it will then check; here there is exactly one file to check, so the walk
// follows that file's own import graph instead and never lists a directory it wasn't
// pointed at. Both phases after that — external-package tracing and
// `finalise_index`'s cross-file resolution — are shared verbatim, since both are folds
// over whatever file set they're handed.
//
// Returns `None` when there is no project root at all (no `pyproject.toml` anywhere
// above the file). That is not an error: without a root there is no place to resolve a
// project-local import against and no `.venv` to find installed packages in, so the
// caller falls back to the same no-index behaviour it had before, having genuinely
// tried first.
pub(crate) fn build_single_file_index_internal(file_path: &Path) -> Option<ProjectIndex> {
    let project_root = find_project_root_opt(file_path)?;
    let config = load_linter_config(&project_root);
    let excluded_dirs = resolve_excluded_dirs(&config);

    let mut files: HashMap<String, IndexEntry> = HashMap::new();
    let mut discovered_candidates: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    // Breadth-first, so the closest (and most likely relevant) files are the ones that
    // get indexed if MAX_SINGLE_FILE_INDEXED_FILES cuts the walk short.
    let mut queue: std::collections::VecDeque<(PathBuf, usize)> =
        std::collections::VecDeque::from([(file_path.to_path_buf(), 0)]);
    let mut seen: std::collections::HashSet<PathBuf> =
        std::collections::HashSet::from([file_path.to_path_buf()]);

    while let Some((current, depth)) = queue.pop_front() {
        if files.len() >= MAX_SINGLE_FILE_INDEXED_FILES {
            break;
        }
        let Some((entry, candidates)) = index_file(&current, &project_root, &config) else {
            continue;
        };
        discovered_candidates.extend(candidates);
        // Only absolute imports are followed. A relative import (`from .schemas import
        // UserSchema`) is skipped by `index_file` itself and, more to the point, by
        // `load_cross_file_symbols` at check time — so its target could never be looked
        // up even if it were indexed here.
        if depth < MAX_SINGLE_FILE_IMPORT_DEPTH {
            let imported_modules = entry.imports.values().chain(entry.module_aliases.values());
            for module_name in imported_modules {
                let Some(target) =
                    resolve_first_party_module_path(module_name, &project_root, &excluded_dirs)
                else {
                    continue;
                };
                if seen.insert(target.clone()) {
                    queue.push_back((target, depth + 1));
                }
            }
        }
        if let Some(path_str) = current.to_str() {
            files.insert(path_str.to_string(), entry);
        }
    }

    index_traced_external_packages(&project_root, &config, discovered_candidates, &mut files);
    Some(finalise_index(&project_root, files))
}

// Resolve each function's `param_schema_name` (the schema its first parameter is
// annotated with, e.g. `Annotated[pl.DataFrame, ReportSchema]`) to that schema's
// actual column list, overriding the heuristic body-scan `requires` with the
// authoritative one. Must run project-wide, after every file is indexed: the schema
// may be defined in a THIRD file — neither the one containing the function nor the
// one importing it — so a single file's own Linter pass (see visit_stmt) often can't
// resolve it locally. Runs before resolve_transitive_requires so that functions
// delegating to a schema-annotated one inherit the authoritative set, not the
// heuristic fallback.
pub(crate) fn resolve_param_schema_requires(
    files: &mut HashMap<String, IndexEntry>,
    all_schemas: &HashMap<String, Vec<String>>,
) {
    for entry in files.values_mut() {
        for func in entry.functions.values_mut() {
            if func.param_schema_name.is_empty() {
                continue;
            }
            if let Some(cols) = all_schemas.get(&func.param_schema_name) {
                let mut cols = cols.clone();
                cols.sort();
                cols.dedup();
                func.requires = cols;
            }
        }
    }
}

// A function identified by the file that defines it and its name within that file.
pub(crate) type FuncNode = (String, String);

// Resolve a delegate call target (a bare name, as written at the call site) to the
// node that actually defines it: a same-file function first, else a function
// reached by following `from_file`'s own `from X import name` statements.
// Resolve a dotted module name (e.g. "pkg.transforms") to the project file that
// defines it, trying both the project root and a `src/` layout — same candidate
// paths used throughout this module for cross-file resolution.
pub(crate) fn resolve_module_file(
    module_name: &str,
    project_root: &Path,
    files: &HashMap<String, IndexEntry>,
) -> Option<String> {
    let mod_path = module_name.replace('.', "/");
    let mut candidates = vec![
        project_root.join(format!("{mod_path}.py")),
        project_root.join("src").join(format!("{mod_path}.py")),
    ];
    // An explicitly allowlisted external package (see collect_external_package_files)
    // is indexed into the same `files` map under its real site-packages path — these
    // two extra candidates are how an import of it actually gets found.
    if let Some(site_packages) = find_site_packages_dir(project_root) {
        candidates.push(site_packages.join(format!("{mod_path}.py")));
        candidates.push(site_packages.join(&mod_path).join("__init__.py"));
    }
    candidates
        .iter()
        .filter_map(|p| p.to_str())
        .find(|p| files.contains_key(*p))
        .map(str::to_string)
}

// Resolve a delegate call target (a bare name, as written at the call site) to the
// node that actually defines it: a same-file function first, else a function
// reached by following `from_file`'s own `from X import name` statements, else —
// since attribute-style calls (`transforms.enrich(df)`) are recorded as the bare
// attribute name `enrich` with no module prefix (see scan_expr_for_contract) — a
// function by that name in any module `from_file` plainly `import`s. That last
// step can't distinguish which plainly-imported module a given call actually used
// if more than one defines a same-named function; same conservative trade-off as
// the rest of this heuristic (see the cycle-handling note on resolve_node_requires).
pub(crate) fn resolve_delegate_target(
    from_file: &str,
    callee: &str,
    project_root: &Path,
    files: &HashMap<String, IndexEntry>,
) -> Option<FuncNode> {
    let entry = files.get(from_file)?;
    if entry.functions.contains_key(callee) {
        return Some((from_file.to_string(), callee.to_string()));
    }
    if let Some(module_name) = entry.imports.get(callee) {
        let target_file = resolve_module_file(module_name, project_root, files)?;
        if files.get(&target_file)?.functions.contains_key(callee) {
            return Some((target_file, callee.to_string()));
        }
    }
    for module_name in entry.module_aliases.values() {
        let Some(target_file) = resolve_module_file(module_name, project_root, files) else {
            continue;
        };
        if files.get(&target_file)?.functions.contains_key(callee) {
            return Some((target_file, callee.to_string()));
        }
    }
    None
}

// Line/column for a source offset, without needing a full `Linter` instance — used by
// resolve_param_governed_call_sites, which parses each file fresh outside any Linter's
// own `check_file_internal` pass.
pub(crate) fn compute_source_location(
    source: &str,
    offset: ruff_text_size::TextSize,
) -> (usize, usize) {
    let line_index = LineIndex::from_source_text(source);
    let source_code = SourceCode::new(source, &line_index);
    let loc = source_code.line_column(offset);
    (loc.line.get(), loc.column.get())
}

// Project-wide pass: for every function that `find_param_governed_feast_template`
// found a template for, find every call site (anywhere in the project, at the same
// statement-nesting depth `analyze_stmt_for_contract` already covers) and check it
// against the template's recorded accesses — independently per call site, so two
// callers passing different arguments for the same parameter get diagnosed (or not)
// completely independently. A call site whose argument doesn't trace to a literal
// (a variable with no traceable origin, a dynamically-built list, ...) gets its own
// untracked-dataframe diagnostic right there, rather than falling back to the callee's
// generic one — the callee's own shape is exactly as resolvable as any other governed
// function; the ambiguity genuinely originates at the call site that couldn't produce
// a literal, so that's where the diagnostic belongs.
//
// Diagnostics are returned keyed by the *calling* file's path — not the callee's file
// — which is what lets this avoid ever needing two different diagnoses for the same
// source line: the callee's own access line is one, single, caller-independent AST
// location, and every different-per-caller outcome instead lives at that caller's own
// (necessarily distinct) call-site location.
pub(crate) struct GovernedCallSiteResult {
    pub(crate) call_site_errors: HashMap<String, Vec<LintError>>,
    pub(crate) resolved_governed: std::collections::HashSet<(String, String)>,
}

pub(crate) fn resolve_param_governed_call_sites(
    project_root: &Path,
    files: &HashMap<String, IndexEntry>,
) -> GovernedCallSiteResult {
    let mut result = GovernedCallSiteResult {
        call_site_errors: HashMap::new(),
        resolved_governed: std::collections::HashSet::new(),
    };
    let has_any_governed = files
        .values()
        .any(|entry| entry.functions.values().any(|f| f.param_governed.is_some()));
    if !has_any_governed {
        return result;
    }

    for file_path in files.keys() {
        let Ok(source) = fs::read_to_string(file_path) else {
            continue;
        };
        let Ok(parsed) = parse_module(&source) else {
            continue;
        };
        let module = parsed.into_syntax();
        scan_stmts_for_governed_calls(
            &module.body,
            file_path,
            &source,
            project_root,
            files,
            &mut result,
        );
    }
    result
}

pub(crate) fn scan_stmts_for_governed_calls(
    stmts: &[Stmt],
    file_path: &str,
    source: &str,
    project_root: &Path,
    files: &HashMap<String, IndexEntry>,
    result: &mut GovernedCallSiteResult,
) {
    for stmt in stmts {
        let call = match stmt {
            Stmt::Expr(expr_stmt) => match &*expr_stmt.value {
                Expr::Call(c) => Some(c),
                _ => None,
            },
            Stmt::Assign(assign) => match &*assign.value {
                Expr::Call(c) => Some(c),
                _ => None,
            },
            _ => None,
        };
        if let Some(call) = call {
            check_governed_call_site(call, file_path, source, project_root, files, result);
        }
        match stmt {
            Stmt::If(if_stmt) => {
                scan_stmts_for_governed_calls(
                    &if_stmt.body,
                    file_path,
                    source,
                    project_root,
                    files,
                    result,
                );
                for clause in &if_stmt.elif_else_clauses {
                    scan_stmts_for_governed_calls(
                        &clause.body,
                        file_path,
                        source,
                        project_root,
                        files,
                        result,
                    );
                }
            }
            Stmt::For(for_stmt) => {
                scan_stmts_for_governed_calls(
                    &for_stmt.body,
                    file_path,
                    source,
                    project_root,
                    files,
                    result,
                );
            }
            Stmt::While(while_stmt) => {
                scan_stmts_for_governed_calls(
                    &while_stmt.body,
                    file_path,
                    source,
                    project_root,
                    files,
                    result,
                );
            }
            Stmt::With(with_stmt) => {
                scan_stmts_for_governed_calls(
                    &with_stmt.body,
                    file_path,
                    source,
                    project_root,
                    files,
                    result,
                );
            }
            _ => {}
        }
    }
}

pub(crate) fn check_governed_call_site(
    call: &ast::ExprCall,
    file_path: &str,
    source: &str,
    project_root: &Path,
    files: &HashMap<String, IndexEntry>,
    result: &mut GovernedCallSiteResult,
) {
    let callee_name = match &*call.func {
        Expr::Name(n) => n.id.as_str(),
        Expr::Attribute(attr) => attr.attr.as_str(),
        _ => return,
    };
    let Some((target_file, target_func)) =
        resolve_delegate_target(file_path, callee_name, project_root, files)
    else {
        return;
    };
    let Some(func) = files
        .get(&target_file)
        .and_then(|e| e.functions.get(&target_func))
    else {
        return;
    };
    let Some(template) = &func.param_governed else {
        return;
    };

    let arg_expr = call
        .arguments
        .keywords
        .iter()
        .find(|k| k.arg.as_ref().map(|s| s.as_str()) == Some(template.param_name.as_str()))
        .map(|k| &k.value)
        .or_else(|| call.arguments.args.get(template.param_index));
    let Some(arg_expr) = arg_expr else {
        return; // not passed at this call site (e.g. a default value) -- nothing to check
    };
    // A general lazy evaluator: a direct literal list, a literal/f-string built from an
    // environment of substituted parameter values, or a call to another function --
    // whose own argument expressions are evaluated in the CURRENT (empty, at the call
    // site) environment, bound to the callee's parameters, and followed recursively
    // through its first return statement, as many hops as needed with cycle
    // protection. This is strictly more general than a zero-arg forward: a caller
    // doesn't have to pass the literal directly, or even forward a zero-arg call --
    // `helper("driver_stats")` is followed by substituting "driver_stats" for helper's
    // own parameter and evaluating its f-string return with it.
    let raw_items = eval_feast_list_expr(
        arg_expr,
        &HashMap::new(),
        file_path,
        project_root,
        files,
        &mut std::collections::HashSet::new(),
    );
    let resolved_cols = raw_items.and_then(|raw| {
        ast_extract::feast_columns_from_raw_items(&raw, template.full_feature_names)
    });

    // This call site is real -- it passes SOMETHING for the governed parameter, whether
    // or not that something is traceable to a literal -- so the callee's own generic
    // "columns unknown at lint time" framing is stale either way: either this call site
    // resolves (validated right here, below), or it doesn't, in which case the SAME
    // untracked-dataframe signal is reported HERE instead of inside the callee, since
    // call-site tracing means this call site is where the real ambiguity actually
    // originates -- the callee's own shape is exactly as resolvable as any other
    // call-site-governed function, in the abstract. check_file consults this set either
    // way to retract the callee's own line.
    result
        .resolved_governed
        .insert((target_file.clone(), target_func.clone()));

    let (line, col) = compute_source_location(source, call.range().start());

    let Some(resolved_cols) = resolved_cols else {
        result
            .call_site_errors
            .entry(file_path.to_string())
            .or_default()
            .push(LintError {
                line,
                col,
                code: CODE_UNTRACKED_DATAFRAME.to_string(),
                message: format!(
                    "columns unknown at lint time; the `{}` argument passed here isn't \
                     a literal (or traceable to one), so column access inside '{}' can't \
                     be validated for this call",
                    template.param_name, target_func
                ),
                severity: "warning".to_string(),
            });
        return;
    };

    for access in &template.accesses {
        if !resolved_cols.iter().any(|c| c == &access.column) {
            result
                .call_site_errors
                .entry(file_path.to_string())
                .or_default()
                .push(LintError {
                    line,
                    col,
                    code: CODE_UNKNOWN_COLUMN.to_string(),
                    message: format!(
                        "Column '{}' does not exist for this call's resolved features {:?} \
                     (accessed at {}:{}:{} inside '{}')",
                        access.column,
                        resolved_cols,
                        target_file,
                        access.line,
                        access.col,
                        target_func
                    ),
                    severity: "error".to_string(),
                });
        }
    }
}

// Memoised DFS over the delegate graph: a function's fully resolved requirement set
// is its own direct requirements plus the union of every delegate's (transitively)
// resolved requirements. `visiting` guards against infinite recursion on a cycle
// (mutually- or self-delegating functions) — a node caught in a cycle contributes
// only its direct requirements to the cycle, rather than looping forever.
pub(crate) fn resolve_node_requires(
    node: &FuncNode,
    project_root: &Path,
    files: &HashMap<String, IndexEntry>,
    memo: &mut HashMap<FuncNode, Vec<String>>,
    visiting: &mut std::collections::HashSet<FuncNode>,
) -> Vec<String> {
    if let Some(cached) = memo.get(node) {
        return cached.clone();
    }
    if visiting.contains(node) {
        return files
            .get(&node.0)
            .and_then(|e| e.functions.get(&node.1))
            .map(|f| f.requires.clone())
            .unwrap_or_default();
    }
    let Some(func) = files.get(&node.0).and_then(|e| e.functions.get(&node.1)) else {
        return Vec::new();
    };

    visiting.insert(node.clone());
    let mut result = func.requires.clone();
    for delegate_name in &func.delegates {
        if let Some(target) = resolve_delegate_target(&node.0, delegate_name, project_root, files) {
            let target_reqs = resolve_node_requires(&target, project_root, files, memo, visiting);
            for col in target_reqs {
                if !result.contains(&col) {
                    result.push(col);
                }
            }
        }
    }
    visiting.remove(node);

    result.sort();
    result.dedup();
    memo.insert(node.clone(), result.clone());
    result
}

// Resolve every function's `requires` to the transitive union of everything it
// (directly or indirectly) delegates its own parameter to. Runs once, project-wide,
// after every file has been indexed independently — only then do we know, for every
// file, which functions live where and what each one's direct requirements are.
pub(crate) fn resolve_transitive_requires(
    project_root: &Path,
    files: &mut HashMap<String, IndexEntry>,
) {
    let nodes: Vec<FuncNode> = files
        .iter()
        .flat_map(|(file, entry)| {
            entry
                .functions
                .keys()
                .map(move |f| (file.clone(), f.clone()))
        })
        .collect();

    let mut memo: HashMap<FuncNode, Vec<String>> = HashMap::new();
    let mut resolved: Vec<(FuncNode, Vec<String>)> = Vec::new();
    for node in nodes {
        let files_ref: &HashMap<String, IndexEntry> = files;
        let mut visiting = std::collections::HashSet::new();
        let r = resolve_node_requires(&node, project_root, files_ref, &mut memo, &mut visiting);
        resolved.push((node, r));
    }

    for ((file, func), reqs) in resolved {
        if let Some(entry) = files.get_mut(&file) {
            if let Some(f) = entry.functions.get_mut(&func) {
                f.requires = reqs;
            }
        }
    }
}

// General lazy evaluator for a feature-list-producing expression: a literal list, a
// literal string/f-string built from an environment of substituted parameter values,
// or a call to another function -- in which case the call's own argument expressions
// are evaluated in the CURRENT environment, bound to the callee's parameter names, and
// the callee's first return statement is evaluated recursively in that new
// environment. This subsumes direct literals, zero-arg forwarding, AND argument
// substitution in one general mechanism. `visiting` breaks cycles (mutually- or
// self-delegating chains) the same way resolve_node_requires does elsewhere.
pub(crate) fn eval_feast_list_expr(
    expr: &Expr,
    env: &HashMap<String, String>,
    file: &str,
    project_root: &Path,
    files: &HashMap<String, IndexEntry>,
    visiting: &mut std::collections::HashSet<FuncNode>,
) -> Option<Vec<String>> {
    match expr {
        Expr::List(list) => list
            .elts
            .iter()
            .map(|e| eval_feast_string_expr(e, env))
            .collect(),
        Expr::Call(call) => eval_feast_call(call, env, file, project_root, files, visiting),
        _ => None,
    }
}

pub(crate) fn eval_feast_string_expr(expr: &Expr, env: &HashMap<String, String>) -> Option<String> {
    match expr {
        Expr::StringLiteral(s) => Some(s.value.to_str().to_string()),
        Expr::Name(n) => env.get(n.id.as_str()).cloned(),
        Expr::FString(f) => eval_fstring_expr(f, env),
        _ => None,
    }
}

pub(crate) fn eval_fstring_expr(
    fstring: &ast::ExprFString,
    env: &HashMap<String, String>,
) -> Option<String> {
    let mut result = String::new();
    for part in fstring.value.as_slice() {
        match part {
            ast::FStringPart::Literal(lit) => result.push_str(&lit.value),
            ast::FStringPart::FString(f) => {
                for element in f.elements.iter() {
                    match element {
                        ast::InterpolatedStringElement::Literal(lit) => {
                            result.push_str(&lit.value);
                        }
                        ast::InterpolatedStringElement::Interpolation(interp) => {
                            if interp.conversion != ast::ConversionFlag::None
                                || interp.format_spec.is_some()
                            {
                                return None;
                            }
                            let Expr::Name(name) = &*interp.expression else {
                                return None;
                            };
                            result.push_str(env.get(name.id.as_str())?);
                        }
                    }
                }
            }
        }
    }
    Some(result)
}

pub(crate) fn eval_feast_call(
    call: &ast::ExprCall,
    env: &HashMap<String, String>,
    file: &str,
    project_root: &Path,
    files: &HashMap<String, IndexEntry>,
    visiting: &mut std::collections::HashSet<FuncNode>,
) -> Option<Vec<String>> {
    if !call.arguments.keywords.is_empty() {
        return None;
    }
    let callee_name = match &*call.func {
        Expr::Name(n) => n.id.as_str(),
        Expr::Attribute(a) => a.attr.as_str(),
        _ => return None,
    };
    let (target_file, target_func) =
        resolve_delegate_target(file, callee_name, project_root, files)?;
    let node = (target_file.clone(), target_func.clone());
    if visiting.contains(&node) {
        return None;
    }

    let mut arg_values = Vec::with_capacity(call.arguments.args.len());
    for arg in &call.arguments.args {
        arg_values.push(eval_feast_string_expr(arg, env)?);
    }

    let source = fs::read_to_string(&target_file).ok()?;
    let parsed = parse_module(&source).ok()?;
    let module = parsed.into_syntax();
    let func_def = find_function_def_by_name(&module.body, &target_func)?;

    let param_names: Vec<String> = func_def
        .parameters
        .posonlyargs
        .iter()
        .chain(func_def.parameters.args.iter())
        .map(|p| p.parameter.name.id.to_string())
        .collect();
    if param_names.len() != arg_values.len() {
        return None;
    }
    let new_env: HashMap<String, String> = param_names.into_iter().zip(arg_values).collect();

    let return_expr = find_first_return_expr(&func_def.body)?;

    visiting.insert(node.clone());
    let result = eval_feast_list_expr(
        return_expr,
        &new_env,
        &target_file,
        project_root,
        files,
        visiting,
    );
    visiting.remove(&node);
    result
}

pub(crate) fn find_function_def_by_name<'a>(
    stmts: &'a [Stmt],
    name: &str,
) -> Option<&'a ast::StmtFunctionDef> {
    for stmt in stmts {
        match stmt {
            Stmt::FunctionDef(f) if f.name.as_str() == name => return Some(f),
            Stmt::If(if_stmt) => {
                if let Some(f) = find_function_def_by_name(&if_stmt.body, name) {
                    return Some(f);
                }
                for clause in &if_stmt.elif_else_clauses {
                    if let Some(f) = find_function_def_by_name(&clause.body, name) {
                        return Some(f);
                    }
                }
            }
            Stmt::For(for_stmt) => {
                if let Some(f) = find_function_def_by_name(&for_stmt.body, name) {
                    return Some(f);
                }
            }
            Stmt::While(while_stmt) => {
                if let Some(f) = find_function_def_by_name(&while_stmt.body, name) {
                    return Some(f);
                }
            }
            Stmt::With(with_stmt) => {
                if let Some(f) = find_function_def_by_name(&with_stmt.body, name) {
                    return Some(f);
                }
            }
            _ => {}
        }
    }
    None
}

pub(crate) fn find_first_return_expr(stmts: &[Stmt]) -> Option<&Expr> {
    for stmt in stmts {
        match stmt {
            Stmt::Return(ret) => {
                if let Some(v) = &ret.value {
                    return Some(v);
                }
            }
            Stmt::If(if_stmt) => {
                if let Some(e) = find_first_return_expr(&if_stmt.body) {
                    return Some(e);
                }
                for clause in &if_stmt.elif_else_clauses {
                    if let Some(e) = find_first_return_expr(&clause.body) {
                        return Some(e);
                    }
                }
            }
            Stmt::For(for_stmt) => {
                if let Some(e) = find_first_return_expr(&for_stmt.body) {
                    return Some(e);
                }
            }
            Stmt::While(while_stmt) => {
                if let Some(e) = find_first_return_expr(&while_stmt.body) {
                    return Some(e);
                }
            }
            Stmt::With(with_stmt) => {
                if let Some(e) = find_first_return_expr(&with_stmt.body) {
                    return Some(e);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LinterConfig;
    use std::path::Path;

    #[test]
    fn test_should_cap_auto_discovered_candidates_but_never_force_included_ones() {
        // arrange: 25 auto-discovered candidates (more than MAX_AUTO_TRACED_PACKAGES),
        // plus one package named in trace_external_packages that would sort dead last
        // alphabetically -- it must still appear in the result, uncapped.
        let config = LinterConfig {
            enabled: None,
            warnings: None,
            sql_dialect: None,
            trace_external_packages: Some(vec!["zzz_forced_pkg".to_string()]),
            excluded_external_packages: None,
            exclude: None,
        };
        let candidates: std::collections::HashSet<String> =
            (0..25).map(|i| format!("pkg_{i:02}")).collect();
        let dir = tempfile::tempdir().unwrap();

        // act
        let result =
            resolve_traced_external_packages(&config, candidates, dir.path(), &HashMap::new());

        // assert: forced package present regardless of the cap ...
        assert!(result.contains(&"zzz_forced_pkg".to_string()));
        // ... and the auto-discovered portion is capped at MAX_AUTO_TRACED_PACKAGES,
        // keeping the alphabetically-first candidates for determinism.
        let auto_portion: Vec<&String> = result
            .iter()
            .filter(|name| *name != "zzz_forced_pkg")
            .collect();
        assert_eq!(auto_portion.len(), MAX_AUTO_TRACED_PACKAGES);
        assert!(auto_portion.contains(&&"pkg_00".to_string()));
        assert!(!auto_portion.contains(&&format!("pkg_{:02}", 24)));
    }

    #[test]
    fn test_should_respect_excluded_external_packages_in_resolve_traced_external_packages() {
        // arrange
        let config = LinterConfig {
            enabled: None,
            warnings: None,
            sql_dialect: None,
            trace_external_packages: None,
            excluded_external_packages: Some(vec!["untrusted_pkg".to_string()]),
            exclude: None,
        };
        let candidates: std::collections::HashSet<String> =
            ["untrusted_pkg".to_string(), "fine_pkg".to_string()]
                .into_iter()
                .collect();
        let dir = tempfile::tempdir().unwrap();

        // act
        let result =
            resolve_traced_external_packages(&config, candidates, dir.path(), &HashMap::new());

        // assert
        assert!(!result.contains(&"untrusted_pkg".to_string()));
        assert!(result.contains(&"fine_pkg".to_string()));
    }

    #[test]
    fn test_should_never_treat_a_first_party_module_as_an_external_candidate() {
        // arrange: "utils" resolves to a real first-party file already in the index --
        // a bare `import utils` looking momentarily unresolved (before cross-file
        // symbols are loaded) must never be treated as an external-tracing candidate.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let config = LinterConfig::EMPTY;
        let mut first_party_files = HashMap::new();
        first_party_files.insert(
            root.join("utils.py").to_str().unwrap().to_string(),
            IndexEntry {
                schemas: HashMap::new(),
                schema_locations: HashMap::new(),
                functions: HashMap::new(),
                exports: Vec::new(),
                imports: HashMap::new(),
                module_aliases: HashMap::new(),
                class_methods: HashMap::new(),
            },
        );
        let candidates: std::collections::HashSet<String> =
            ["utils".to_string()].into_iter().collect();

        // act
        let result =
            resolve_traced_external_packages(&config, candidates, root, &first_party_files);

        // assert
        assert!(result.is_empty());
    }

    #[test]
    fn test_should_not_hang_on_mutually_delegating_functions() {
        // arrange: f and g delegate to each other — a cycle in the delegate graph.
        // Resolution must terminate (cycle guard) rather than recursing forever.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("pyproject.toml"), "").unwrap();
        fs::write(
            root.join("steps.py"),
            r#"
import pandas as pd

def f(df: pd.DataFrame) -> pd.DataFrame:
    x = df["a"]
    return g(df)

def g(df: pd.DataFrame) -> pd.DataFrame:
    y = df["b"]
    return f(df)
"#,
        )
        .unwrap();

        // act
        let index = build_index_internal(root);

        // assert: both functions resolved without hanging, each contributing at
        // least its own direct requirement.
        let steps_path = root.join("steps.py").to_str().unwrap().to_string();
        let entry = index.files.get(&steps_path).expect("steps.py indexed");
        let f_requires = &entry.functions.get("f").expect("f indexed").requires;
        let g_requires = &entry.functions.get("g").expect("g indexed").requires;
        assert!(f_requires.contains(&"a".to_string()));
        assert!(g_requires.contains(&"b".to_string()));
    }

    #[test]
    fn test_should_prune_configured_exclude_directories_from_project_index() {
        // A non-default directory (e.g. a vendored `third_party/`) isn't caught by
        // DEFAULT_EXCLUDED_DIRS -- [tool.typedframes] exclude is what lets a project
        // prune it (or any other named directory) explicitly.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(
            root.join("pyproject.toml"),
            "[tool.typedframes]\nexclude = [\"third_party\"]",
        )
        .unwrap();
        fs::create_dir(root.join("third_party")).unwrap();
        fs::write(
            root.join("third_party").join("vendored.py"),
            r#"
from typedframes import BaseSchema, Column

class VendoredSchema(BaseSchema):
    x = Column(type=int)
"#,
        )
        .unwrap();
        fs::write(root.join("app.py"), "x = 1\n").unwrap();

        // act
        let index = build_index_internal(root);

        // assert -- only app.py was indexed; third_party/ was pruned entirely
        let indexed: Vec<&String> = index.files.keys().collect();
        assert!(
            indexed.iter().any(|p| p.ends_with("app.py")),
            "{indexed:#?}"
        );
        assert!(
            !indexed.iter().any(|p| p.contains("third_party")),
            "third_party/ should have been pruned: {indexed:#?}"
        );
        assert!(!index.all_schemas.contains_key("VendoredSchema"));
    }

    #[test]
    fn test_should_prune_dot_claude_by_default_with_no_config_at_all() {
        // .claude is in DEFAULT_EXCLUDED_DIRS -- pruned automatically with no
        // [tool.typedframes] exclude configured at all, same as .venv/.git/etc.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join(".claude").join("worktrees").join("agent-1")).unwrap();
        fs::write(
            root.join(".claude")
                .join("worktrees")
                .join("agent-1")
                .join("stale.py"),
            r#"
from typedframes import BaseSchema, Column

class StaleSchema(BaseSchema):
    x = Column(type=int)
"#,
        )
        .unwrap();
        fs::write(root.join("app.py"), "x = 1\n").unwrap();

        // act
        let index = build_index_internal(root);

        // assert
        let indexed: Vec<&String> = index.files.keys().collect();
        assert!(
            indexed.iter().any(|p| p.ends_with("app.py")),
            "{indexed:#?}"
        );
        assert!(
            !indexed.iter().any(|p| p.contains(".claude")),
            ".claude/ should have been pruned by default: {indexed:#?}"
        );
        assert!(!index.all_schemas.contains_key("StaleSchema"));
    }

    #[test]
    fn test_should_replace_rather_than_add_to_default_excludes_when_configured() {
        // Configuring [tool.typedframes] exclude REPLACES DEFAULT_EXCLUDED_DIRS
        // entirely -- it does not add to it. A project that configures its own
        // exclude list without re-listing .venv gets .venv walked again; that's the
        // deliberate override semantics (matching ruff's own exclude, as opposed to
        // extend-exclude), not a bug.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(
            root.join("pyproject.toml"),
            "[tool.typedframes]\nexclude = [\"custom_dir\"]",
        )
        .unwrap();
        fs::create_dir(root.join("custom_dir")).unwrap();
        fs::write(root.join("custom_dir").join("skipped.py"), "x = 1\n").unwrap();
        fs::create_dir(root.join(".venv")).unwrap();
        fs::write(
            root.join(".venv").join("walked.py"),
            r#"
from typedframes import BaseSchema, Column

class VenvSchema(BaseSchema):
    x = Column(type=int)
"#,
        )
        .unwrap();

        // act
        let index = build_index_internal(root);

        // assert
        let indexed: Vec<&String> = index.files.keys().collect();
        assert!(
            !indexed.iter().any(|p| p.contains("custom_dir")),
            "custom_dir/ should have been pruned (in the configured exclude list): {indexed:#?}"
        );
        assert!(
            indexed.iter().any(|p| p.contains(".venv")),
            ".venv/ should NOT be pruned once exclude is configured without re-listing \
             it -- override, not union: {indexed:#?}"
        );
        assert!(index.all_schemas.contains_key("VenvSchema"));
    }

    #[test]
    fn test_should_resolve_param_governed_call_site_across_files() {
        // arrange: load_conv_rate is defined in helpers.py; pipeline.py calls it twice
        // with different literals, same as the same-file test above, but now resolved
        // through resolve_delegate_target's cross-file import resolution.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("pyproject.toml"), "").unwrap();
        fs::write(
            root.join("helpers.py"),
            r#"
from feast import FeatureStore
import pandas as pd


def load_conv_rate(store: FeatureStore, entity_df: pd.DataFrame, feature_names: list) -> None:
    df = store.get_historical_features(entity_df=entity_df, features=feature_names).to_df()
    print(df["conv_rate"])
"#,
        )
        .unwrap();
        let pipeline_source = r#"
from helpers import load_conv_rate

load_conv_rate(store, entity_df, ["driver_stats:conv_rate"])
load_conv_rate(store, entity_df, ["driver_stats:acc_rate"])
"#;
        let pipeline_path = root.join("pipeline.py");
        fs::write(&pipeline_path, pipeline_source).unwrap();

        // act
        let index = build_index_internal(root);
        let pipeline_path_str = pipeline_path.to_str().unwrap().to_string();

        // assert
        let errors = index
            .call_site_errors
            .get(&pipeline_path_str)
            .cloned()
            .unwrap_or_default();
        assert_eq!(errors.len(), 1, "errors: {errors:#?}");
        assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
        assert_eq!(errors[0].line, 5, "should be the SECOND call site only");
        assert!(errors[0].message.contains("conv_rate"));
        assert!(errors[0].message.contains("helpers.py"));
    }

    #[test]
    fn test_should_resolve_call_site_argument_through_a_multi_hop_delegate_chain() {
        // A call site doesn't have to pass the literal directly -- it can pass a call
        // to a zero-arg helper, which itself just forwards to ANOTHER helper, which
        // finally returns the literal. resolve_returns_literal_list must follow that
        // whole chain (helper_a -> helper_b -> literal), not just one hop.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("pyproject.toml"), "").unwrap();
        let source = r#"
from feast import FeatureStore
import pandas as pd


def load_conv_rate(store: FeatureStore, entity_df: pd.DataFrame, feature_names: list) -> None:
    df = store.get_historical_features(entity_df=entity_df, features=feature_names).to_df()
    print(df["conv_rate"])


def helper_b() -> list[str]:
    return ["driver_stats:acc_rate"]


def helper_a() -> list[str]:
    return helper_b()


load_conv_rate(store, entity_df, helper_a())
"#;
        let file_path = root.join("pipeline.py");
        fs::write(&file_path, source).unwrap();

        // act
        let index = build_index_internal(root);

        // assert: helper_a resolves (through helper_b) to {acc_rate}, which does NOT
        // satisfy load_conv_rate's print(df["conv_rate"]) -- caught two hops away.
        let file_path_str = file_path.to_str().unwrap().to_string();
        let errors = index
            .call_site_errors
            .get(&file_path_str)
            .cloned()
            .unwrap_or_default();
        assert_eq!(errors.len(), 1, "errors: {errors:#?}");
        assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
        assert!(errors[0].message.contains("conv_rate"));
        assert!(
            index
                .resolved_governed
                .contains(&(file_path_str, "load_conv_rate".to_string())),
            "load_conv_rate should be resolved_governed via the multi-hop chain"
        );
    }

    #[test]
    fn test_should_resolve_call_site_argument_through_literal_substitution_into_an_fstring() {
        // A call site can pass a call like `feature_names("driver_stats")` -- a
        // genuine argument, not a zero-arg forward -- whose callee builds its return
        // value with an f-string that interpolates that very parameter. The evaluator
        // must substitute the literal "driver_stats" for the callee's own parameter
        // and evaluate the f-string with it, arriving at the same resolved feature list
        // as if the call site had written the literal out directly.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("pyproject.toml"), "").unwrap();
        let source = r#"
from feast import FeatureStore
import pandas as pd


def load_conv_rate(store: FeatureStore, entity_df: pd.DataFrame, feature_names: list) -> None:
    df = store.get_historical_features(entity_df=entity_df, features=feature_names).to_df()
    print(df["conv_rate"])


def feature_names(prefix: str) -> list[str]:
    return [f"{prefix}:conv_rate"]


load_conv_rate(store, entity_df, feature_names("driver_stats"))
"#;
        let file_path = root.join("pipeline.py");
        fs::write(&file_path, source).unwrap();

        // act
        let index = build_index_internal(root);

        // assert: feature_names("driver_stats") resolves (via argument substitution
        // into the f-string) to {"driver_stats:conv_rate"}, which DOES satisfy
        // load_conv_rate's print(df["conv_rate"]) -- no error, and the callee's own
        // untracked-dataframe warning is retracted.
        let file_path_str = file_path.to_str().unwrap().to_string();
        let errors = index
            .call_site_errors
            .get(&file_path_str)
            .cloned()
            .unwrap_or_default();
        assert!(errors.is_empty(), "errors: {errors:#?}");
        assert!(
            index
                .resolved_governed
                .contains(&(file_path_str, "load_conv_rate".to_string())),
            "load_conv_rate should be resolved_governed via argument substitution"
        );
    }

    #[test]
    fn test_should_not_hang_on_a_self_delegating_return_chain() {
        // Recursion protection: a function whose `return` shape delegates to itself
        // (or a cycle of functions delegating to each other) must resolve to
        // unresolvable rather than looping forever -- mirrors
        // test_should_not_hang_on_mutually_delegating_functions's existing coverage
        // for the *requires* side of this same cycle-protection pattern.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("pyproject.toml"), "").unwrap();
        let source = r#"
from feast import FeatureStore
import pandas as pd


def load_conv_rate(store: FeatureStore, entity_df: pd.DataFrame, feature_names: list) -> None:
    df = store.get_historical_features(entity_df=entity_df, features=feature_names).to_df()
    print(df["conv_rate"])


def cyclic_a() -> list[str]:
    return cyclic_b()


def cyclic_b() -> list[str]:
    return cyclic_a()


load_conv_rate(store, entity_df, cyclic_a())
"#;
        let file_path = root.join("pipeline.py");
        fs::write(&file_path, source).unwrap();

        // act -- must terminate (this test itself hanging is the failure mode)
        let index = build_index_internal(root);

        // assert: the argument is unresolvable (the cycle guard gives up), but the
        // call site itself was still seen -- so its own untracked-dataframe
        // diagnostic is reported right there, and load_conv_rate is in
        // resolved_governed so the callee's own generic line is retracted.
        let file_path_str = file_path.to_str().unwrap().to_string();
        let errors = index
            .call_site_errors
            .get(&file_path_str)
            .cloned()
            .unwrap_or_default();
        assert_eq!(errors.len(), 1, "errors: {errors:#?}");
        assert_eq!(errors[0].code, CODE_UNTRACKED_DATAFRAME);
        assert!(index
            .resolved_governed
            .contains(&(file_path_str, "load_conv_rate".to_string())));
    }

    #[test]
    fn test_should_resolve_transitive_requires_through_plain_import_delegate() {
        // arrange: transform() only forwards df to steps.postproc(df) via a plain
        // `import steps` — the delegate-detection blind spot for attribute calls.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("pyproject.toml"), "").unwrap();
        fs::write(
            root.join("steps.py"),
            r#"
import pandas as pd

def postproc(df: pd.DataFrame) -> pd.DataFrame:
    y = df["c"]
    return df
"#,
        )
        .unwrap();
        fs::write(
            root.join("transforms.py"),
            r#"
import steps

def transform(df):
    return steps.postproc(df)
"#,
        )
        .unwrap();

        // act
        let index = build_index_internal(root);
        let transforms_path = root.join("transforms.py").to_str().unwrap().to_string();
        let func = &index.files[&transforms_path].functions["transform"];

        // assert: transform's own requires is the transitive union through the
        // attribute-style delegate call, i.e. {c} — not empty, which is what it
        // would be if the `steps.postproc(df)` call were never recognised as a
        // delegate at all.
        assert_eq!(func.requires, vec!["c".to_string()]);
    }

    // pipeline.py -> loaders.py -> schemas.py: the schema the checked file's diagnostics
    // depend on is TWO import hops away, in a file pipeline.py never names itself. Shared
    // by the single-file tests below, which differ only in what they then assert.
    fn write_two_hop_project(root: &Path) -> PathBuf {
        fs::write(root.join("pyproject.toml"), "[tool.typedframes]\n").unwrap();
        fs::write(
            root.join("schemas.py"),
            r#"
from typedframes import BaseSchema, Column


class UserSchema(BaseSchema):
    user_id = Column(type=int)
    name = Column(type=str)
"#,
        )
        .unwrap();
        fs::write(
            root.join("loaders.py"),
            r#"
from typing import Annotated

import pandas as pd

from schemas import UserSchema


def load_users(path: str) -> Annotated[pd.DataFrame, UserSchema]:
    return pd.read_csv(path)
"#,
        )
        .unwrap();
        let pipeline_path = root.join("pipeline.py");
        fs::write(
            &pipeline_path,
            r#"
from loaders import load_users


def run(path: str) -> None:
    df = load_users(path)
    print(df["name"])
"#,
        )
        .unwrap();
        pipeline_path
    }

    #[test]
    fn test_should_resolve_a_schema_two_import_hops_away_from_a_single_checked_file() {
        // arrange: pipeline.py imports load_users from loaders.py, whose return
        // annotation names UserSchema -- defined in a THIRD file pipeline.py never
        // mentions. Following only direct imports would miss it.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let pipeline_path = write_two_hop_project(root);

        // act
        let index = build_single_file_index_internal(&pipeline_path).expect("project root found");

        // assert: the two-hop schema resolved, with the location that makes the
        // "(defined at ...)" part of a diagnostic possible.
        assert_eq!(
            index.all_schemas.get("UserSchema"),
            Some(&vec!["name".to_string(), "user_id".to_string()])
        );
        assert!(index.all_schema_locations.contains_key("UserSchema"));
    }

    #[test]
    fn test_should_not_index_project_files_the_checked_file_never_references() {
        // The whole point of single-file mode: cost proportional to what the checked
        // file references, not to what the project contains. An unrelated file sitting
        // right next to pipeline.py must never be opened.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let pipeline_path = write_two_hop_project(root);
        fs::write(
            root.join("unrelated.py"),
            r#"
from typedframes import BaseSchema, Column


class UnrelatedSchema(BaseSchema):
    other = Column(type=int)
"#,
        )
        .unwrap();

        // act
        let index = build_single_file_index_internal(&pipeline_path).expect("project root found");

        // assert
        let indexed: Vec<&String> = index.files.keys().collect();
        assert_eq!(indexed.len(), 3, "{indexed:#?}");
        assert!(
            !indexed.iter().any(|p| p.ends_with("unrelated.py")),
            "unrelated.py is not referenced by pipeline.py and must not be indexed: {indexed:#?}"
        );
        assert!(!index.all_schemas.contains_key("UnrelatedSchema"));
        // ... and for contrast, the whole-project index DOES pick it up -- the two
        // modes differ in file set, which is exactly the intended trade-off.
        assert!(build_index_internal(root)
            .all_schemas
            .contains_key("UnrelatedSchema"));
    }

    #[test]
    fn test_should_stop_following_a_single_file_import_chain_past_the_depth_bound() {
        // arrange: a straight chain hop0 (the checked file) -> hop1 -> ... -> hop5.
        // MAX_SINGLE_FILE_IMPORT_DEPTH hops past the checked file get indexed; the
        // next one does not.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("pyproject.toml"), "[tool.typedframes]\n").unwrap();
        let chain_len = MAX_SINGLE_FILE_IMPORT_DEPTH + 3;
        for i in 0..chain_len {
            fs::write(
                root.join(format!("hop{i}.py")),
                format!("from hop{} import thing_{}\n", i + 1, i + 1),
            )
            .unwrap();
        }
        fs::write(
            root.join(format!("hop{chain_len}.py")),
            format!("thing_{chain_len} = 1\n"),
        )
        .unwrap();

        // act
        let index = build_single_file_index_internal(&root.join("hop0.py")).expect("root found");

        // assert
        let indexed: Vec<&String> = index.files.keys().collect();
        assert_eq!(
            indexed.len(),
            MAX_SINGLE_FILE_IMPORT_DEPTH + 1,
            "{indexed:#?}"
        );
        let last_followed = format!("hop{MAX_SINGLE_FILE_IMPORT_DEPTH}.py");
        let first_dropped = format!("hop{}.py", MAX_SINGLE_FILE_IMPORT_DEPTH + 1);
        assert!(indexed.iter().any(|p| p.ends_with(&last_followed)));
        assert!(!indexed.iter().any(|p| p.ends_with(&first_dropped)));
    }

    #[test]
    fn test_should_cap_the_number_of_files_a_single_file_import_walk_indexes() {
        // arrange: one file importing far more modules than the cap allows, so the
        // depth bound alone never fires -- only MAX_SINGLE_FILE_INDEXED_FILES stops it.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("pyproject.toml"), "[tool.typedframes]\n").unwrap();
        let fan_out = MAX_SINGLE_FILE_INDEXED_FILES + 50;
        let mut imports = String::new();
        for i in 0..fan_out {
            fs::write(
                root.join(format!("wide_{i}.py")),
                format!("value_{i} = 1\n"),
            )
            .unwrap();
            imports.push_str(&format!("from wide_{i} import value_{i}\n"));
        }
        let hub_path = root.join("hub.py");
        fs::write(&hub_path, imports).unwrap();

        // act
        let index = build_single_file_index_internal(&hub_path).expect("root found");

        // assert
        assert_eq!(index.files.len(), MAX_SINGLE_FILE_INDEXED_FILES);
    }

    #[test]
    fn test_should_follow_a_single_file_import_into_a_src_layout() {
        // A `src/` layout is the second root resolve_module_file tries at check time,
        // so single-file discovery has to try it too or the file it finds and the file
        // check time looks for would disagree.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("pyproject.toml"), "[tool.typedframes]\n").unwrap();
        fs::create_dir(root.join("src")).unwrap();
        fs::write(
            root.join("src").join("schemas.py"),
            r#"
from typedframes import BaseSchema, Column


class SrcSchema(BaseSchema):
    amount = Column(type=int)
"#,
        )
        .unwrap();
        let pipeline_path = root.join("src").join("pipeline.py");
        fs::write(&pipeline_path, "from schemas import SrcSchema\n").unwrap();

        // act
        let index = build_single_file_index_internal(&pipeline_path).expect("root found");

        // assert
        assert!(index.all_schemas.contains_key("SrcSchema"));
    }

    #[test]
    fn test_should_not_follow_a_single_file_import_into_an_excluded_directory() {
        // A configured `exclude` prunes the same directories here as it does for the
        // whole-project walk -- an import that happens to resolve into one is dropped
        // rather than quietly reintroducing what the user asked to ignore.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(
            root.join("pyproject.toml"),
            "[tool.typedframes]\nexclude = [\"vendor\"]\n",
        )
        .unwrap();
        fs::create_dir(root.join("vendor")).unwrap();
        fs::write(
            root.join("vendor").join("schemas.py"),
            r#"
from typedframes import BaseSchema, Column


class VendoredSchema(BaseSchema):
    x = Column(type=int)
"#,
        )
        .unwrap();
        let pipeline_path = root.join("pipeline.py");
        fs::write(
            &pipeline_path,
            "from vendor.schemas import VendoredSchema\n",
        )
        .unwrap();

        // act
        let index = build_single_file_index_internal(&pipeline_path).expect("root found");

        // assert
        assert_eq!(index.files.len(), 1, "only pipeline.py itself");
        assert!(!index.all_schemas.contains_key("VendoredSchema"));
    }

    #[test]
    fn test_should_auto_discover_an_external_package_from_a_single_checked_file() {
        // arrange: the same shape as linter.rs's py.typed auto-discovery coverage, but
        // reached from ONE file rather than a whole-project walk -- the import sits
        // right there in the checked file, so it must still be followed.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("pyproject.toml"), "[tool.typedframes]\n").unwrap();
        let site_packages = root
            .join(".venv")
            .join("lib")
            .join("python3.12")
            .join("site-packages");
        let pkg_dir = site_packages.join("internal_repo_pkg");
        fs::create_dir_all(&pkg_dir).unwrap();
        fs::write(pkg_dir.join("py.typed"), "").unwrap();
        fs::write(
            pkg_dir.join("__init__.py"),
            r#"
import pandas as pd


class DataRepository:
    def get(self, query: str) -> pd.DataFrame:
        return pd.read_sql(query, None)
"#,
        )
        .unwrap();
        let pipeline_path = root.join("pipeline.py");
        fs::write(
            &pipeline_path,
            r#"
from internal_repo_pkg import DataRepository


class Pipeline:
    def __init__(self):
        self._data_repository = DataRepository()

    def run(self):
        df = self._data_repository.get("SELECT * FROM training")
        return df
"#,
        )
        .unwrap();

        // act
        let index = build_single_file_index_internal(&pipeline_path).expect("root found");

        // assert: the installed package's own file was indexed alongside the checked
        // one, which is what makes DataRepository.get's return type resolvable.
        let indexed: Vec<&String> = index.files.keys().collect();
        assert!(
            indexed
                .iter()
                .any(|p| p.contains("internal_repo_pkg") && p.ends_with("__init__.py")),
            "{indexed:#?}"
        );
        let pkg_entry = index
            .files
            .get(pkg_dir.join("__init__.py").to_str().unwrap())
            .expect("package indexed");
        assert!(pkg_entry.class_methods.contains_key("DataRepository"));
    }

    #[test]
    fn test_should_skip_single_file_indexing_when_there_is_no_project_root() {
        // A lone script with no pyproject.toml anywhere above it has no root to
        // resolve imports against and no .venv to find packages in. Skipping is the
        // documented graceful outcome (the caller falls back to no index at all), not
        // an error.
        let dir = tempfile::tempdir().unwrap();
        let lone = dir.path().join("lone_script.py");
        fs::write(&lone, "from schemas import UserSchema\n").unwrap();

        // act / assert
        assert!(build_single_file_index_internal(&lone).is_none());
    }

    #[test]
    fn test_should_resolve_a_param_governed_call_site_in_single_file_mode() {
        // The call-site pass (resolve_param_governed_call_sites) is a fold over the
        // file map it's handed, so it works unchanged on a single file's import
        // closure -- here the governed callee lives one hop away in helpers.py.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("pyproject.toml"), "[tool.typedframes]\n").unwrap();
        fs::write(
            root.join("helpers.py"),
            r#"
from feast import FeatureStore
import pandas as pd


def load_conv_rate(store: FeatureStore, entity_df: pd.DataFrame, feature_names: list) -> None:
    df = store.get_historical_features(entity_df=entity_df, features=feature_names).to_df()
    print(df["conv_rate"])
"#,
        )
        .unwrap();
        let pipeline_path = root.join("pipeline.py");
        fs::write(
            &pipeline_path,
            r#"
from helpers import load_conv_rate

load_conv_rate(store, entity_df, ["driver_stats:acc_rate"])
"#,
        )
        .unwrap();

        // act
        let index = build_single_file_index_internal(&pipeline_path).expect("root found");

        // assert
        let errors = index
            .call_site_errors
            .get(pipeline_path.to_str().unwrap())
            .cloned()
            .unwrap_or_default();
        assert_eq!(errors.len(), 1, "errors: {errors:#?}");
        assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
        assert!(errors[0].message.contains("conv_rate"));
    }

    fn write_import_heavy_project(root: &Path, num_files: usize) {
        fs::write(root.join("pyproject.toml"), "[tool.typedframes]\n").unwrap();
        for hub in 0..5 {
            fs::write(
                root.join(format!("hub{hub}.py")),
                format!(
                    r#"
import pandas as pd

def load_hub{hub}(path: str) -> pd.DataFrame:
    return pd.read_csv(path, usecols=["a", "b", "c"])
"#
                ),
            )
            .unwrap();
        }
        for i in 0..num_files {
            let hub = i % 5;
            fs::write(
                root.join(format!("mod_{i}.py")),
                format!(
                    r#"
from hub{hub} import load_hub{hub}

def process_{i}(path: str) -> None:
    df = load_hub{hub}(path)
    print(df["a"])
"#
                ),
            )
            .unwrap();
        }
    }

    #[test]
    #[ignore]
    fn bench_build_index_without_venv() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_import_heavy_project(root, 300);

        let start = std::time::Instant::now();
        let index = build_index_internal(root);
        let elapsed = start.elapsed();
        eprintln!(
            "build_index_internal, 300 files, no .venv: {:?} ({} files indexed)",
            elapsed,
            index.files.len()
        );
    }

    #[test]
    #[ignore]
    fn bench_build_index_with_venv_present_but_unused() {
        // Same project, but with a real (populated-looking) .venv present --
        // trace_external_packages is NOT set, so no external package should actually
        // be indexed, but every import resolution still probes for a site-packages
        // dir (cached after the first call -- this measures whether that caching
        // actually keeps the cost negligible for the common case where the feature
        // isn't in use at all).
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_import_heavy_project(root, 300);
        let site_packages = root
            .join(".venv")
            .join("lib")
            .join("python3.12")
            .join("site-packages");
        fs::create_dir_all(&site_packages).unwrap();
        // A handful of unrelated installed packages, so the directory listing this
        // benchmark exercises isn't unrealistically empty.
        for pkg in ["numpy", "pandas", "requests", "urllib3", "certifi"] {
            fs::create_dir_all(site_packages.join(pkg)).unwrap();
            fs::write(site_packages.join(pkg).join("__init__.py"), "").unwrap();
        }

        let start = std::time::Instant::now();
        let index = build_index_internal(root);
        let elapsed = start.elapsed();
        eprintln!(
            "build_index_internal, 300 files, .venv present (unused): {:?} ({} files indexed)",
            elapsed,
            index.files.len()
        );
    }

    #[test]
    #[ignore]
    fn bench_build_index_with_venv_and_allowlisted_package() {
        // Same as above, but trace_external_packages actually names one of the fake
        // installed packages -- measures the added cost of actually indexing an
        // external package's files, not just probing for the directory.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_import_heavy_project(root, 300);
        let site_packages = root
            .join(".venv")
            .join("lib")
            .join("python3.12")
            .join("site-packages");
        fs::create_dir_all(&site_packages).unwrap();
        for pkg in ["numpy", "pandas", "requests", "urllib3", "certifi"] {
            fs::create_dir_all(site_packages.join(pkg)).unwrap();
            fs::write(site_packages.join(pkg).join("__init__.py"), "").unwrap();
        }
        let internal_pkg = site_packages.join("internal_snowflake_pkg");
        fs::create_dir_all(&internal_pkg).unwrap();
        fs::write(
            internal_pkg.join("__init__.py"),
            r#"
import pandas as pd

def load_orders(query: str) -> pd.DataFrame:
    return pd.read_csv(query, usecols=["order_id", "amount"])
"#,
        )
        .unwrap();
        fs::write(
            root.join("pyproject.toml"),
            "[tool.typedframes]\ntrace_external_packages = [\"internal_snowflake_pkg\"]\n",
        )
        .unwrap();

        let start = std::time::Instant::now();
        let index = build_index_internal(root);
        let elapsed = start.elapsed();
        eprintln!(
            "build_index_internal, 300 files, .venv + allowlisted package: {:?} ({} files indexed)",
            elapsed,
            index.files.len()
        );
    }

    #[test]
    #[ignore]
    fn bench_single_file_vs_whole_project_index() {
        // The trade-off single-file mode exists to make: the same project, indexed
        // whole vs. indexed from one file's own import closure. mod_0.py imports
        // exactly one hub module, so its closure is 2 files out of 305.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_import_heavy_project(root, 300);

        let start = std::time::Instant::now();
        let whole = build_index_internal(root);
        let whole_elapsed = start.elapsed();

        let target = root.join("mod_0.py");
        let start = std::time::Instant::now();
        let single = build_single_file_index_internal(&target).expect("root found");
        let single_elapsed = start.elapsed();

        eprintln!(
            "305-file project: whole-project index {:?} ({} files) vs single-file index \
             {:?} ({} files)",
            whole_elapsed,
            whole.files.len(),
            single_elapsed,
            single.files.len()
        );
    }

    #[test]
    #[ignore]
    fn bench_single_file_index_worst_case_fan_out() {
        // The honest worst case for single-file mode: a file that imports enough
        // distinct modules, each importing more, to hit MAX_SINGLE_FILE_INDEXED_FILES.
        // Bounded by construction, but this is what "bounded" actually costs.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("pyproject.toml"), "[tool.typedframes]\n").unwrap();
        let fan_out = MAX_SINGLE_FILE_INDEXED_FILES + 50;
        let mut imports = String::new();
        for i in 0..fan_out {
            fs::write(
                root.join(format!("wide_{i}.py")),
                format!(
                    r#"
import pandas as pd

def load_{i}(path: str) -> pd.DataFrame:
    return pd.read_csv(path, usecols=["a", "b", "c"])
"#
                ),
            )
            .unwrap();
            imports.push_str(&format!("from wide_{i} import load_{i}\n"));
        }
        let hub_path = root.join("hub.py");
        fs::write(&hub_path, imports).unwrap();

        let start = std::time::Instant::now();
        let index = build_single_file_index_internal(&hub_path).expect("root found");
        let elapsed = start.elapsed();
        eprintln!(
            "single-file index, worst-case fan-out (capped): {:?} ({} files indexed of \
             {fan_out} importable)",
            elapsed,
            index.files.len()
        );
    }

    #[test]
    #[ignore]
    fn bench_find_site_packages_dir_repeated_calls_uncached() {
        // Directly measures what EVERY resolve_module_file call used to pay before
        // SITE_PACKAGES_CACHE was added -- one read_dir per call, simulating roughly
        // one import statement's worth of resolution per iteration.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let site_packages = root
            .join(".venv")
            .join("lib")
            .join("python3.12")
            .join("site-packages");
        fs::create_dir_all(&site_packages).unwrap();
        for pkg in ["numpy", "pandas", "requests", "urllib3", "certifi"] {
            fs::create_dir_all(site_packages.join(pkg)).unwrap();
        }

        let iterations = 1000;
        let start = std::time::Instant::now();
        for _ in 0..iterations {
            let _ = find_site_packages_dir_uncached(root);
        }
        let elapsed = start.elapsed();
        eprintln!(
            "find_site_packages_dir_uncached x{iterations}: {:?} total, {:?}/call",
            elapsed,
            elapsed / iterations
        );

        let start = std::time::Instant::now();
        for _ in 0..iterations {
            let _ = find_site_packages_dir(root);
        }
        let elapsed = start.elapsed();
        eprintln!(
            "find_site_packages_dir (cached) x{iterations}: {:?} total, {:?}/call",
            elapsed,
            elapsed / iterations
        );
    }
}

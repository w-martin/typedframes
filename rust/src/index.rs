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
use crate::{Linter, ParamGovernedTemplate};
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

// Collect `.py` files for each explicitly allowlisted external package (see
// LinterConfig::trace_external_packages), resolved from the project's own
// site-packages directory. Deliberately narrow: only the allowlisted packages' own
// directories are walked — never the whole site-packages tree, which would be both
// expensive and a far larger, unbounded trust surface than an explicit opt-in list.
pub(crate) fn collect_external_package_files(
    project_root: &Path,
    config: &LinterConfig,
) -> Vec<PathBuf> {
    let Some(packages) = config.trace_external_packages.as_ref() else {
        return Vec::new();
    };
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
pub(crate) fn index_file(
    path: &Path,
    project_root: &Path,
    config: &LinterConfig,
) -> Option<IndexEntry> {
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

    Some(IndexEntry {
        schemas,
        schema_locations,
        functions,
        exports,
        imports,
        module_aliases,
    })
}

// Build a ProjectIndex by indexing every `.py` file under `project_root`.
pub(crate) fn build_index_internal(project_root: &Path) -> ProjectIndex {
    // Loaded once and threaded into every per-file `Linter`, so index-time inference
    // (this function) and check-time inference (`check_file`) agree on `sql_dialect` —
    // a mismatch here would make a SQL-derived schema's columns differ depending on
    // whether they were read from the cached project index or inferred fresh.
    let config = load_linter_config(project_root);
    // A configured `exclude` REPLACES DEFAULT_EXCLUDED_DIRS entirely -- it does not
    // add to it (see collect_py_files's doc comment).
    let resolved_excludes: std::collections::HashSet<String> = match &config.exclude {
        Some(list) => list.iter().cloned().collect(),
        None => DEFAULT_EXCLUDED_DIRS
            .iter()
            .map(|s| s.to_string())
            .collect(),
    };
    let mut py_files = collect_py_files(project_root, &resolved_excludes);
    py_files.extend(collect_external_package_files(project_root, &config));
    let mut files = HashMap::new();
    for file_path in py_files {
        if let Some(entry) = index_file(&file_path, project_root, &config) {
            if let Some(path_str) = file_path.to_str() {
                files.insert(path_str.to_string(), entry);
            }
        }
    }
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

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

mod sql;

use pyo3::prelude::*;
use ruff_python_ast::visitor::{self as ast_visitor, Visitor};
use ruff_python_ast::{self as ast, Expr, Stmt};
use ruff_python_parser::parse_module;
use ruff_source_file::{LineIndex, SourceCode};
use ruff_text_size::Ranged;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
/// Check a single Python file for DataFrame column errors.
///
/// Accepts an optional MessagePack-serialised [`ProjectIndex`] (produced by
/// [`build_project_index`]) so the linter can resolve cross-file imports, e.g. a schema
/// defined in `schemas.py` and used in `pipeline.py`.  Returns a JSON array of
/// [`LintError`] objects, or `"[]"` when the linter is disabled in `pyproject.toml`.
#[pyfunction]
#[pyo3(signature = (file_path, index_bytes = None))]
fn check_file(file_path: String, index_bytes: Option<Vec<u8>>) -> PyResult<String> {
    let path = Path::new(&file_path);
    let project_root = find_project_root(path);
    let config = load_linter_config(&project_root);

    if !config.enabled.unwrap_or(true) {
        let empty = CheckFileResult {
            errors: Vec::new(),
            stats: FileStats::default(),
        };
        return serde_json::to_string(&empty)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("{}", e)));
    }

    let source = fs::read_to_string(path)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyIOError, _>(format!("{}", e)))?;

    let mut linter = Linter::new();
    linter.with_context(project_root.clone(), &config);

    if let Some(bytes) = index_bytes {
        if let Some(index) = get_cached_index(&bytes) {
            linter.load_cross_file_symbols(&index, &source, path, &project_root);
        }
    }

    let mut errors = linter
        .check_file_internal(&source, path)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("{}", e)))?;

    if !config.warnings.unwrap_or(true) {
        errors.retain(|e| e.severity != "warning");
    }

    let result = CheckFileResult {
        errors,
        stats: FileStats {
            dataframes_total: linter.dataframes_total,
            dataframes_typed: linter.dataframes_typed,
        },
    };

    serde_json::to_string(&result)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("{}", e)))
}

// The CLI calls check_file once per file in a project, passing the SAME serialised
// index_bytes on every call (see `_check_files` in cli.py, and mypy.py's per-file
// hook, which does the same across a single mypy run). Deserialising a project-wide
// index — which can be hundreds of KB for a large project, since it must carry every
// candidate delegate-target function, not just DataFrame-relevant ones — on every
// single file turns an O(1)-per-file operation into an O(project size)-per-file one,
// i.e. O(files^2) for a whole-project check. Cache the deserialised index keyed by a
// hash of its bytes (hashing is far cheaper than re-deserialising nested
// Strings/Vecs/HashMaps) so repeat calls with the same index are a cache hit; a
// different hash (e.g. a different project root between calls) correctly replaces
// the cached entry rather than silently reusing stale data.
static INDEX_CACHE: Mutex<Option<(u64, Arc<ProjectIndex>)>> = Mutex::new(None);

fn get_cached_index(bytes: &[u8]) -> Option<Arc<ProjectIndex>> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    let hash = hasher.finish();

    let mut cache = INDEX_CACHE.lock().ok()?;
    if let Some((cached_hash, index)) = cache.as_ref() {
        if *cached_hash == hash {
            return Some(index.clone());
        }
    }
    let index = Arc::new(rmp_serde::from_slice::<ProjectIndex>(bytes).ok()?);
    *cache = Some((hash, index.clone()));
    Some(index)
}

/// Build a cross-file symbol index for an entire project.
///
/// Walks all `.py` files under `project_root`, parses each one, and extracts
/// schema definitions and annotated function return types into a [`ProjectIndex`].
/// The index is serialised with MessagePack and returned as raw bytes so it can be
/// held in Python memory and passed to subsequent [`check_file`] calls without any
/// intermediate disk I/O.
#[pyfunction]
fn build_project_index(project_root: String) -> PyResult<Vec<u8>> {
    let root = Path::new(&project_root);
    let index = build_index_internal(root);
    rmp_serde::to_vec(&index)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("{}", e)))
}

#[pymodule]
fn _rust_checker(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(check_file, m)?)?;
    m.add_function(wrap_pyfunction!(build_project_index, m)?)?;
    Ok(())
}

// Root deserialisation target for `pyproject.toml`.
#[derive(serde::Deserialize)]
struct Config {
    tool: Option<ToolConfig>,
}

// `[tool]` section of `pyproject.toml`.
#[derive(serde::Deserialize)]
struct ToolConfig {
    typedframes: Option<LinterConfig>,
}

// `[tool.typedframes]` configuration block.
// All fields are optional; absent keys default as documented on each field.
#[derive(serde::Deserialize)]
pub struct LinterConfig {
    enabled: Option<bool>,  // default: true
    warnings: Option<bool>, // default: true
    // Dialect name used to fold unquoted SQL identifier case when inferring columns
    // from a literal SELECT list (e.g. "snowflake", "postgres", "bigquery"). Unknown or
    // absent values fall back to `SqlDialect::Generic` (no folding) — see
    // `SqlDialect::from_config_str`.
    sql_dialect: Option<String>,
    // Explicit allowlist of installed (non-project) package names whose own
    // Annotated[...]/BaseSchema declarations and recognized transform patterns should
    // be indexed the same way first-party files are -- e.g. an internal company
    // package that wraps a SQL connector. Resolved from the project's own auto-detected
    // `.venv` site-packages directory (see `find_site_packages_dir`); no editable-install
    // support and no path override in this version. Deliberately an explicit allowlist
    // rather than indexing all of site-packages, which would be both expensive and a
    // much larger, unbounded trust surface.
    trace_external_packages: Option<Vec<String>>,
}

impl LinterConfig {
    const EMPTY: Self = Self {
        enabled: None,
        warnings: None,
        sql_dialect: None,
        trace_external_packages: None,
    };
}

// Read `[tool.typedframes]` from `pyproject.toml` at `project_root`.
// Returns a config with all fields `None` if the file is absent, unreadable, or has no
// `[tool.typedframes]` section; callers use `.unwrap_or(true)` on each field.
fn load_linter_config(project_root: &Path) -> LinterConfig {
    let config_path = project_root.join("pyproject.toml");
    if !config_path.exists() {
        return LinterConfig::EMPTY;
    }

    let content = match fs::read_to_string(config_path) {
        Ok(c) => c,
        Err(_) => return LinterConfig::EMPTY,
    };

    let config: Config = match toml::from_str(&content) {
        Ok(c) => c,
        Err(_) => return LinterConfig::EMPTY,
    };

    config
        .tool
        .and_then(|t| t.typedframes)
        .unwrap_or(LinterConfig::EMPTY)
}

/// Return `true` if the linter is enabled for `project_root` (default: `true`).
pub fn is_enabled(project_root: &Path) -> bool {
    load_linter_config(project_root).enabled.unwrap_or(true)
}

/// Walk up the directory tree from `start_path` until a `pyproject.toml` is found.
///
/// Returns the directory containing `pyproject.toml`, or `start_path` itself if no
/// `pyproject.toml` exists anywhere in the ancestor chain (e.g. standalone scripts).
pub fn find_project_root(start_path: &Path) -> PathBuf {
    let mut current = start_path.to_path_buf();
    if current.is_file() {
        current.pop();
    }
    loop {
        if current.join("pyproject.toml").exists() {
            return current;
        }
        if !current.pop() {
            return start_path.to_path_buf();
        }
    }
}

// ── Index structs ──────────────────────────────────────────────────────────────

// Return-type and parameter-contract information extracted from a function definition.
#[derive(Serialize, Deserialize)]
struct IndexFunction {
    returns_schema: String, // BaseSchema subclass name, e.g. "OrderSchema"; empty if none
    returns_frame: String,  // reserved for future use
    // Columns required on the function's first parameter. Starts as the columns
    // accessed *directly* in the function body; resolve_transitive_requires (run once,
    // project-wide, in build_index_internal) folds in the requirements of every
    // function this one delegates its own parameter to, so by the time an importer
    // reads this field it already reflects the full transitive union.
    requires: Vec<String>,
    def_line: usize, // line the function is defined on, for origin messages
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
    delegates: Vec<String>,
    // Schema name from the first parameter's annotation, e.g. "ReportSchema" from
    // `Annotated[pl.DataFrame, ReportSchema]`; empty if the parameter isn't
    // schema-annotated. Captured at index time regardless of whether the schema is
    // resolvable yet (it may be defined in a third file) — resolve_transitive_requires
    // resolves it project-wide and overrides `requires` with the authoritative column
    // list once every file's schemas are known, superseding the heuristic body-scan.
    // `#[serde(skip)]`: same reasoning as `delegates` above — dead weight past
    // resolve_param_schema_requires, must not be serialised into ProjectIndex.
    #[serde(skip)]
    param_schema_name: String,
}

// Symbol table for a single `.py` file, stored inside ProjectIndex.
#[derive(Serialize, Deserialize)]
struct IndexEntry {
    schemas: HashMap<String, Vec<String>>, // schema name -> column list
    functions: HashMap<String, IndexFunction>, // function name -> return type info
    exports: Vec<String>,                  // names in __all__, for wildcard-import resolution
    imports: HashMap<String, String>, // imported name -> dotted module it came from (`from X import Y`)
    // Local alias -> dotted module name, from plain `import module [as alias]`
    // statements. Lets resolve_delegate_target and load_cross_file_symbols follow
    // attribute-style calls (`transforms.enrich(df)`) back to the module they came
    // from, the same way `imports` does for `from transforms import enrich`.
    module_aliases: HashMap<String, String>,
}

// In-memory cross-file symbol index.
// Serialised as MessagePack so it can be held in Python memory and passed to check_file
// without any intermediate disk I/O.  The version field allows future format migrations.
#[derive(Serialize, Deserialize)]
struct ProjectIndex {
    version: u32,                       // format version, currently always 1
    files: HashMap<String, IndexEntry>, // absolute file path -> IndexEntry
    // Project-wide schema name -> column list, unioned across every file's `schemas`
    // map. Computed once here in build_index_internal so that per-file lookups (see
    // Linter::load_cross_file_symbols, called once per file by `check_file`) are a
    // single field read instead of a rebuild-by-iterating-every-file — the latter
    // turned a per-file O(1) lookup into an O(files) one, i.e. O(files^2) for a whole
    // project check. A schema may be defined in a THIRD file — neither the function's
    // own file nor the one importing it — so this must stay project-wide rather than
    // scoped to a single IndexEntry.
    all_schemas: HashMap<String, Vec<String>>,
}

// Union schema name -> column list across every file's `schemas` map. First
// definition wins on name collision (matches the prior per-call behaviour in both
// resolve_param_schema_requires and load_cross_file_symbols).
fn compute_all_schemas(files: &HashMap<String, IndexEntry>) -> HashMap<String, Vec<String>> {
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

// ── Index helpers ──────────────────────────────────────────────────────────────

// Recursively collect all `.py` files under `dir`, skipping hidden entries (`.venv`,
// `.git`, etc.).  Uses an explicit stack rather than recursion to avoid stack overflow
// on very deep trees.
fn collect_py_files(dir: &Path) -> Vec<PathBuf> {
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
            if name_str.starts_with('.') {
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
static SITE_PACKAGES_CACHE: Mutex<Option<(PathBuf, Option<PathBuf>)>> = Mutex::new(None);

fn find_site_packages_dir(project_root: &Path) -> Option<PathBuf> {
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

fn find_site_packages_dir_uncached(project_root: &Path) -> Option<PathBuf> {
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
fn collect_external_package_files(project_root: &Path, config: &LinterConfig) -> Vec<PathBuf> {
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
            result.extend(collect_py_files(&package_dir));
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
fn index_file(path: &Path, project_root: &Path, config: &LinterConfig) -> Option<IndexEntry> {
    let source = fs::read_to_string(path).ok()?;

    let mut linter = Linter::new();
    linter.with_context(project_root.to_path_buf(), config);
    let _ = linter.check_file_internal(&source, path);

    let schemas = linter.schemas;
    // Union function names across the return-schema, requires, delegates, and
    // param-schema-name maps — a function may appear in any subset of the four.
    let mut func_names: std::collections::BTreeSet<String> =
        linter.functions.keys().cloned().collect();
    func_names.extend(linter.requires.keys().cloned());
    func_names.extend(linter.delegates.keys().cloned());
    func_names.extend(linter.param_schema_names.keys().cloned());
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
        functions,
        exports,
        imports,
        module_aliases,
    })
}

// Build a ProjectIndex by indexing every `.py` file under `project_root`.
fn build_index_internal(project_root: &Path) -> ProjectIndex {
    // Loaded once and threaded into every per-file `Linter`, so index-time inference
    // (this function) and check-time inference (`check_file`) agree on `sql_dialect` —
    // a mismatch here would make a SQL-derived schema's columns differ depending on
    // whether they were read from the cached project index or inferred fresh.
    let config = load_linter_config(project_root);
    let mut py_files = collect_py_files(project_root);
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
    resolve_param_schema_requires(&mut files, &all_schemas);
    resolve_transitive_requires(project_root, &mut files);
    ProjectIndex {
        version: 1,
        files,
        all_schemas,
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
fn resolve_param_schema_requires(
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
type FuncNode = (String, String);

// Resolve a delegate call target (a bare name, as written at the call site) to the
// node that actually defines it: a same-file function first, else a function
// reached by following `from_file`'s own `from X import name` statements.
// Resolve a dotted module name (e.g. "pkg.transforms") to the project file that
// defines it, trying both the project root and a `src/` layout — same candidate
// paths used throughout this module for cross-file resolution.
fn resolve_module_file(
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
fn resolve_delegate_target(
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

// Memoised DFS over the delegate graph: a function's fully resolved requirement set
// is its own direct requirements plus the union of every delegate's (transitively)
// resolved requirements. `visiting` guards against infinite recursion on a cycle
// (mutually- or self-delegating functions) — a node caught in a cycle contributes
// only its direct requirements to the cycle, rather than looping forever.
fn resolve_node_requires(
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
fn resolve_transitive_requires(project_root: &Path, files: &mut HashMap<String, IndexEntry>) {
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

// ──────────────────────────────────────────────────────────────────────────────
// Diagnostic codes
// ──────────────────────────────────────────────────────────────────────────────

const CODE_UNKNOWN_COLUMN: &str = "unknown-column";
const CODE_RESERVED_NAME: &str = "reserved-name";
const CODE_UNTRACKED_DATAFRAME: &str = "untracked-dataframe";
const CODE_DROPPED_UNKNOWN_COLUMN: &str = "dropped-unknown-column";
const CODE_MISSING_COLUMN: &str = "missing-column";

// Return true if the source line at `line` (1-indexed) carries a
// `# typedframes: ignore` or `# typedframes: ignore[code]` comment.
fn is_line_ignored(source: &str, line: usize, code: &str) -> bool {
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

// ──────────────────────────────────────────────────────────────────────────────

// Reserved pandas/polars method names that shouldn't be used as column names
const RESERVED_METHODS: &[&str] = &[
    "shape",
    "columns",
    "index",
    "iloc",
    "loc",
    "head",
    "tail",
    "describe",
    "info",
    "set_index",
    "merge",
    "concat",
    "join",
    "filter",
    "select",
    "with_columns",
    "group_by",
    "groupby",
    "agg",
    "sort",
    "sort_values",
    "drop",
    "rename",
    "apply",
    "map",
    "pipe",
    "transform",
    "to_pandas",
    "to_df",
    "schema",
    "dtypes",
    "dtype",
    "cast",
    "lazy",
    "collect",
    "to_dict",
    "to_list",
    "to_numpy",
    "to_arrow",
    "write_csv",
    "write_parquet",
    "clone",
    "clear",
    "extend",
    "insert",
    "item",
    "n_chunks",
    "null_count",
    "estimated_size",
    "width",
    "height",
    "rows",
    "row",
    "get_column",
    "get_columns",
    "explode",
    "unnest",
    "pivot",
    "unpivot",
    "melt",
    "sample",
    "slice",
    "limit",
    "unique",
    "n_unique",
    "value_counts",
    "is_empty",
    "is_duplicated",
    "unique_counts",
    "mean",
    "sum",
    "min",
    "max",
    "std",
    "var",
    "median",
    "quantile",
    "fill_null",
    "fill_nan",
    "interpolate",
    "shift",
    "diff",
    "pct_change",
    "rolling",
    "ewm",
    "count",
    "first",
    "last",
    "len",
    "all",
    "any",
    "copy",
    "values",
    "T",
    "axes",
    "empty",
    "ndim",
    "size",
    "keys",
    "items",
    "pop",
    "update",
    "get",
    "add",
    "sub",
    "mul",
    "div",
    "mod",
    "pow",
    "abs",
    "round",
    "floor",
    "ceil",
    "clip",
    "corr",
    "cov",
];

const LOAD_FUNCTIONS: &[&str] = &[
    "read_csv",
    "read_parquet",
    "read_json",
    "read_excel",
    "read_sql",
    "read_sql_query",
    "read_sql_table",
    "read_html",
    "read_feather",
    "read_hdf",
    "read_orc",
    "read_clipboard",
    "read_ndjson",
    "read_avro",
    "read_ipc",
    "scan_csv",
    "scan_parquet",
    "scan_json",
    "scan_ndjson",
    "scan_ipc",
    "read_database",
    "read_database_uri",
    "read_gbq",
];

const LOAD_MODULES: &[&str] = &["pd", "pandas", "pl", "polars"];

// Load functions whose column set lives in a SQL SELECT list rather than a
// usecols/columns/dtype/schema kwarg. `read_sql_table` is deliberately excluded: its
// first positional argument is a table name, not SQL, so attempting to parse it as SQL
// would just fail (harmlessly, but for the wrong reason) rather than being skipped
// because we know better.
const SQL_LOAD_FUNCTIONS: &[&str] = &[
    "read_sql",
    "read_sql_query",
    "read_database",
    "read_database_uri",
    "read_gbq",
];

// Feast FeatureStore methods whose result eventually becomes a DataFrame via `.to_df()`
// — either chained directly, or via an intermediate RetrievalJob/OnlineResponse
// variable (the split form; see `retrieval_jobs`). Not gated on any particular receiver
// name (e.g. `store`) — matched structurally by method name plus a literal `features=`
// keyword, since the receiver is whatever variable the caller's FeatureStore happens to
// be bound to.
const FEAST_RETRIEVAL_METHODS: &[&str] = &["get_historical_features", "get_online_features"];

// The `connectorx` package (conventionally imported `as cx`) exposes a `read_sql`
// function with the SQL text as its SECOND positional argument
// (`cx.read_sql(conn_uri, sql)`) — the reverse of pandas' `pd.read_sql(sql, conn)` —
// so it needs its own argument-position handling rather than reusing
// `extract_sql_literal`. Its own module list, separate from `LOAD_MODULES`
// (pd/pl), since it isn't a DataFrame-library namespace itself.
const CONNECTORX_MODULES: &[&str] = &["connectorx", "cx"];

// DataFrame-materializing method that finalizes a connector call chain into an actual
// DataFrame: google-cloud-bigquery's `.to_dataframe()`, PySpark/Databricks Connect's
// `.toPandas()`, DuckDB's `.df()`/`.pl()`. Only dispatches when the call it's chained
// onto is confirmed to be one of `SQL_PRODUCING_METHODS` — see
// `sql_producing_call_args`, and its call sites for why "any receiver's `.df()`" isn't
// enough on its own (that name in particular is common and unrelated most of the time).
const SQL_FINALIZE_METHODS: &[&str] = &["to_dataframe", "toPandas", "df", "pl"];

// Methods/bare functions whose first positional (or `sql=`/`query=` keyword) argument
// is SQL text, chained into one of `SQL_FINALIZE_METHODS`: `client.query(sql)`
// (BigQuery), `spark.sql(sql)`/`session.sql(sql)` (PySpark/Databricks Connect),
// `duckdb.sql(sql)`/`duckdb.query(sql)`.
const SQL_PRODUCING_METHODS: &[&str] = &["query", "sql"];

// Which family of load a call belongs to — used only to phrase the
// `untracked-dataframe` hint appropriately when extraction fails. Not persisted anywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoadKind {
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
enum CaseFold {
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

const ROW_PASSTHROUGH_METHODS: &[&str] = &[
    "filter",
    "query",
    "head",
    "tail",
    "sample",
    "sort_values",
    "sort",
    "reset_index",
    "nlargest",
    "nsmallest",
    "fillna",
    "dropna",
    "ffill",
    "bfill",
];

// Compute the Levenshtein edit distance between two strings using the Wagner–Fischer
// dynamic-programming algorithm.  matrix[i][j] = minimum single-character edits
// (insert, delete, substitute) to transform a[..i] into b[..j].
// Time: O(|a| × |b|).  Space: O(|a| × |b|).
// Ref: Wagner & Fischer (1974), doi:10.1145/321796.321811
fn levenshtein(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let a_len = a_chars.len();
    let b_len = b_chars.len();
    let mut matrix = vec![vec![0; b_len + 1]; a_len + 1];

    for (i, row) in matrix.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in matrix[0].iter_mut().enumerate() {
        *cell = j;
    }

    for i in 1..=a_len {
        for j in 1..=b_len {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            matrix[i][j] = std::cmp::min(
                std::cmp::min(matrix[i - 1][j] + 1, matrix[i][j - 1] + 1),
                matrix[i - 1][j - 1] + cost,
            );
        }
    }
    matrix[a_len][b_len]
}

// Find the closest candidate to `name` within Levenshtein distance ≤ 2.
// The threshold catches common typos (transposed letters, off-by-one characters) while
// avoiding spurious "did you mean?" hints for completely unrelated names.
fn find_best_match<'a>(name: &str, candidates: &'a [String]) -> Option<&'a str> {
    candidates
        .iter()
        .map(|c| (c, levenshtein(name, c)))
        .filter(|(_, dist)| *dist <= 2)
        .min_by_key(|(_, dist)| *dist)
        .map(|(c, _)| c.as_str())
}

/// A single diagnostic produced by the linter.
///
/// Serialises to JSON for the Python API and to the text/GitHub formats in the CLI.
/// Line and column numbers are 1-indexed to match editor conventions and the output
/// of `ruff_source_file::SourceCode::line_column` via `OneIndexed::get()`.
/// Coverage stats for a single checked file: how many DataFrame origins the
/// linter recognized (`dataframes_total`) and how many of those resolved to a
/// known column set (`dataframes_typed`). This is informational — a low ratio
/// means the check had little to validate, not that the file has fewer
/// problems. See [`Linter::dataframes_total`] for exactly what is counted.
#[derive(Debug, Serialize, Default)]
pub struct FileStats {
    pub dataframes_total: usize,
    pub dataframes_typed: usize,
}

/// The JSON payload returned by [`check_file`]: the diagnostics plus coverage stats.
#[derive(Debug, Serialize)]
struct CheckFileResult {
    errors: Vec<LintError>,
    stats: FileStats,
}

#[derive(Debug, Serialize, PartialEq)]
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

/// AST visitor that tracks DataFrame schemas and validates column access.
///
/// # State model
///
/// The linter maintains three pieces of mutable state as it walks the AST:
///
/// * `schemas` — maps a schema name to its list of known column names.  Schemas are
///   created from `BaseSchema` class definitions, inferred from `usecols=`/`columns=`
///   arguments, and synthesised for intermediate results of method chains
///   (e.g. `df.drop(...)`, `df.rename(...)`).  Inferred schema names are prefixed with
///   `__inferred_` to distinguish them from user-defined ones in error messages.
///
/// * `variables` — maps a variable name to `(schema_name, line_defined)`.  Updated
///   whenever a variable is assigned a DataFrame value.  The `line_defined` is used in
///   error messages to tell the user where the schema was established.
///
/// * `functions` — maps a function name to the schema name it returns, populated from
///   return-type annotations (`-> Annotated[pd.DataFrame, MySchema]`).  Used when a
///   call result is assigned to a new variable.
///
/// # Visitor pattern
///
/// `visit_stmt` handles statement-level nodes (class definitions, assignments, function
/// definitions, `del` statements).  `visit_expr` handles expression-level column access
/// checks (`df["col"]`, `df.col`, `pl.col("col")`).  Both methods recurse into child
/// nodes manually rather than using a trait-based visitor, keeping control flow explicit.
pub struct Linter {
    schemas: HashMap<String, Vec<String>>,
    variables: HashMap<String, (String, usize)>, // var_name -> (schema_name, defined_at_line)
    functions: HashMap<String, String>,          // func_name -> schema_name (from return type)
    schema_origins: HashMap<String, String>,     // inferred schema name -> "func (path:line)"
    requires: HashMap<String, (Vec<String>, usize)>, // func_name -> (direct required cols on 1st param, def line)
    delegates: HashMap<String, Vec<String>>, // func_name -> names called with its own (tainted) param forwarded
    param_requires: HashMap<String, (Vec<String>, String)>, // func_name -> (required cols, origin "func (path:line)")
    param_schema_names: HashMap<String, (String, usize)>, // func_name -> (first-param schema annotation name, def line)
    line_index: Option<LineIndex>,
    source: String,
    file_display: String, // absolute-ish path of the file currently being linted
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
    pub dataframes_typed: usize,
    // Dialect used to fold unquoted SQL identifier case when inferring columns from a
    // literal SELECT list. Defaults to `Generic` (no folding); set from
    // `[tool.typedframes] sql_dialect` in pyproject.toml via `with_context`.
    sql_dialect: sql::SqlDialect,
    // Project root, used to resolve and safety-check `.sql` file reads traced back from
    // a load call (see `read_sql_file`). `None` for standalone/no-config invocations,
    // in which case file-based SQL tracing is skipped entirely rather than guessed at.
    project_root: Option<PathBuf>,
    // Names bound to a plain string literal (or a resolvable `.sql`/text file read —
    // see `resolve_literal_rhs`) exactly once anywhere in the module. Populated by a
    // `StringBindingCollector` pre-pass in `check_file_internal`, before the main
    // statement walk, so that e.g. a module-level `QUERY = "..."` constant is visible
    // no matter where in the file it's used. A name reassigned, conditionally assigned,
    // augmented-assigned, or bound by anything other than a plain literal/file-read is
    // absent from this map entirely — see `StringBindingCollector::record` for the
    // poison-on-any-second-binding policy this relies on.
    string_var_candidates: HashMap<String, String>,
    // Names bound to a resolved SQLAlchemy Core `select(...)` column list — e.g.
    // `stmt = select(Order.id, Order.amount)`. Populated inline during the main
    // top-to-bottom statement walk (unlike `string_var_candidates`, which needs a
    // whole-module pre-pass): resolving a `select(...)` call requires the referenced
    // model's columns to already be in `self.schemas`, which — like every other
    // variable/schema binding this checker tracks — is only guaranteed for a class
    // defined earlier in the same file (or in another file, via the project index).
    // A later reassignment simply overwrites the entry, consistent with how
    // `self.variables` already behaves elsewhere in this checker.
    stmt_var_candidates: HashMap<String, Vec<String>>,
    // Names bound to a Feast `store.get_historical_features(...)`/
    // `get_online_features(...)` result's resolved feature-name columns, BEFORE
    // `.to_df()` is called on it (the split form: `job = store.get_...(...)`, then
    // `df = job.to_df()`). Kept separate from `self.variables`/`self.schemas` — a
    // RetrievalJob/OnlineResponse isn't a DataFrame, so `job["x"]` shouldn't be
    // validated as a column access the way it would be if `job` were registered there.
    // See `register_feast_dataframe` for where this becomes an actual tracked frame.
    retrieval_jobs: HashMap<String, Vec<String>>,
    // Schema names (from `self.schemas`) whose known column list is deliberately
    // incomplete — currently only Feast retrieval results (see
    // `register_feast_dataframe`), whose real output also includes entity_df's join
    // keys and timestamp column, not resolvable in general. `schema_has_column` treats
    // membership against these as always `true`, so `unknown-column` can never
    // false-positive on a real column this checker just doesn't know about.
    open_schemas: std::collections::HashSet<String>,
    // Cursor variable name -> the SQL text most recently passed to `cursor.execute(sql)`
    // (the PEP 249 pattern used by Snowflake, Redshift, and similar connectors), until
    // a later `cursor.fetch_pandas_all()` materializes it into a DataFrame. A second
    // `execute()` on the same cursor overwrites (or, if its argument doesn't resolve,
    // removes) the previous entry — matching real PEP 249 semantics, where a cursor
    // holds exactly one most-recently-executed query at a time.
    cursor_sql: HashMap<String, String>,
}

// Walk `stmts` looking for the first `return <Name>` — handles top-level returns
// and those nested inside `if`/`for`/`while`/`with` bodies.  Returns the variable
// name as an owned String, or None if no bare-name return is found.
fn find_returned_var(stmts: &[Stmt]) -> Option<String> {
    for stmt in stmts {
        match stmt {
            Stmt::Return(ret) => {
                if let Some(value) = &ret.value {
                    if let Expr::Name(name) = value.as_ref() {
                        return Some(name.id.to_string());
                    }
                }
            }
            Stmt::If(if_stmt) => {
                if let Some(var) = find_returned_var(&if_stmt.body) {
                    return Some(var);
                }
                for clause in &if_stmt.elif_else_clauses {
                    if let Some(var) = find_returned_var(&clause.body) {
                        return Some(var);
                    }
                }
            }
            Stmt::For(for_stmt) => {
                if let Some(var) = find_returned_var(&for_stmt.body) {
                    return Some(var);
                }
            }
            Stmt::While(while_stmt) => {
                if let Some(var) = find_returned_var(&while_stmt.body) {
                    return Some(var);
                }
            }
            Stmt::With(with_stmt) => {
                if let Some(var) = find_returned_var(&with_stmt.body) {
                    return Some(var);
                }
            }
            _ => {}
        }
    }
    None
}

// A candidate `string_var_candidates` entry: either resolved to a stable literal value,
// or excluded because more than one binding (of any kind) touched the name. See
// `StringBindingCollector::record`.
enum StringBinding {
    Literal(String),
    Poisoned,
}

// AST visitor that finds every binding site of every name in a module and resolves
// single-binding, string-literal-valued ones. Implements `Visitor` (rather than
// hand-rolling recursion into every statement/expression variant) so that binding forms
// buried inside nested bodies — `if`/`for`/`while`/`with`/`try`/comprehensions/nested
// functions — are covered by the trait's default `walk_*` recursion instead of needing
// to be threaded through by hand.
struct StringBindingCollector<'a> {
    linter: &'a Linter,
    current_file: &'a Path,
    reads_used: u32,
    bindings: HashMap<String, StringBinding>,
}

impl<'a> StringBindingCollector<'a> {
    // Record a binding of `name`. Any name already present (regardless of what it was
    // previously recorded as) is poisoned by this second binding — reassignment,
    // conditional assignment, and augmented assignment all resolve to "exclude" this
    // way without needing to reason about control flow. `resolved: None` means "this
    // binding exists but isn't a literal we can use" (e.g. a for-loop variable, a
    // function parameter, an import) — poisons on first sight too.
    fn record(&mut self, name: &str, resolved: Option<String>) {
        if self.bindings.contains_key(name) {
            self.bindings
                .insert(name.to_string(), StringBinding::Poisoned);
            return;
        }
        self.bindings.insert(
            name.to_string(),
            match resolved {
                Some(s) => StringBinding::Literal(s),
                None => StringBinding::Poisoned,
            },
        );
    }

    // Poison every bare `Name` reachable inside an assignment-target expression —
    // handles tuple/list unpacking and starred targets. Attribute (`obj.x = ...`) and
    // subscript (`d["x"] = ...`) targets don't bind a plain name at all, so they're
    // left alone entirely (neither recorded nor poisoned).
    fn record_target_names(&mut self, target: &Expr) {
        match target {
            Expr::Name(n) => self.record(n.id.as_str(), None),
            Expr::Tuple(t) => {
                for elt in &t.elts {
                    self.record_target_names(elt);
                }
            }
            Expr::List(l) => {
                for elt in &l.elts {
                    self.record_target_names(elt);
                }
            }
            Expr::Starred(s) => self.record_target_names(&s.value),
            _ => {}
        }
    }
}

impl<'a> Visitor<'a> for StringBindingCollector<'a> {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::Assign(assign) => {
                // Only a single bare-Name target is treated as a literal candidate;
                // multi-target assignment (`a = b = "x"`) and destructuring targets
                // poison every name they touch instead of guessing which one "owns"
                // the literal.
                if let [Expr::Name(n)] = assign.targets.as_slice() {
                    let resolved = self.linter.resolve_literal_rhs(
                        &assign.value,
                        self.current_file,
                        &mut self.reads_used,
                    );
                    self.record(n.id.as_str(), resolved);
                } else {
                    for target in &assign.targets {
                        self.record_target_names(target);
                    }
                }
            }
            Stmt::AnnAssign(ann) => {
                // A bare `x: str` with no value binds nothing at runtime — not a
                // binding site at all, so neither recorded nor poisoned.
                if let (Expr::Name(n), Some(value)) = (&*ann.target, &ann.value) {
                    let resolved = self.linter.resolve_literal_rhs(
                        value,
                        self.current_file,
                        &mut self.reads_used,
                    );
                    self.record(n.id.as_str(), resolved);
                }
            }
            Stmt::AugAssign(aug) => self.record_target_names(&aug.target),
            Stmt::For(for_stmt) => self.record_target_names(&for_stmt.target),
            Stmt::Global(g) => {
                for name in &g.names {
                    self.record(name.as_str(), None);
                }
            }
            Stmt::Nonlocal(nl) => {
                for name in &nl.names {
                    self.record(name.as_str(), None);
                }
            }
            Stmt::Delete(del) => {
                for target in &del.targets {
                    self.record_target_names(target);
                }
            }
            Stmt::Import(imp) => {
                for alias in &imp.names {
                    let bound = alias.asname.as_ref().unwrap_or(&alias.name);
                    self.record(bound.as_str(), None);
                }
            }
            Stmt::ImportFrom(imp) => {
                for alias in &imp.names {
                    let bound = alias.asname.as_ref().unwrap_or(&alias.name);
                    self.record(bound.as_str(), None);
                }
            }
            _ => {}
        }
        ast_visitor::walk_stmt(self, stmt);
    }

    fn visit_except_handler(&mut self, handler: &'a ast::ExceptHandler) {
        let ast::ExceptHandler::ExceptHandler(h) = handler;
        if let Some(name) = &h.name {
            self.record(name.as_str(), None);
        }
        ast_visitor::walk_except_handler(self, handler);
    }

    fn visit_parameter(&mut self, parameter: &'a ast::Parameter) {
        self.record(parameter.name.as_str(), None);
        ast_visitor::walk_parameter(self, parameter);
    }

    fn visit_with_item(&mut self, item: &'a ast::WithItem) {
        if let Some(vars) = &item.optional_vars {
            self.record_target_names(vars);
        }
        ast_visitor::walk_with_item(self, item);
    }

    fn visit_comprehension(&mut self, comp: &'a ast::Comprehension) {
        self.record_target_names(&comp.target);
        ast_visitor::walk_comprehension(self, comp);
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        // Walrus (`q := "..."`) always poisons rather than being resolved: it's almost
        // always used for control flow (`if (q := f()):`), and a containing expression
        // that runs more than once (a loop, a comprehension) would make "the" value of
        // `q` not actually stable the way a single top-level assignment's is.
        if let Expr::Named(named) = expr {
            if let Expr::Name(n) = &*named.target {
                self.record(n.id.as_str(), None);
            }
        }
        ast_visitor::walk_expr(self, expr);
    }
}

impl Default for Linter {
    fn default() -> Self {
        Self::new()
    }
}

impl Linter {
    pub fn new() -> Self {
        Self {
            schemas: HashMap::new(),
            variables: HashMap::new(),
            functions: HashMap::new(),
            schema_origins: HashMap::new(),
            requires: HashMap::new(),
            delegates: HashMap::new(),
            param_requires: HashMap::new(),
            param_schema_names: HashMap::new(),
            line_index: None,
            source: String::new(),
            file_display: String::new(),
            dataframes_total: 0,
            dataframes_typed: 0,
            sql_dialect: sql::SqlDialect::Generic,
            project_root: None,
            string_var_candidates: HashMap::new(),
            stmt_var_candidates: HashMap::new(),
            retrieval_jobs: HashMap::new(),
            open_schemas: std::collections::HashSet::new(),
            cursor_sql: HashMap::new(),
        }
    }

    // Apply project-level context resolved by the caller: the project root (for
    // resolving `.sql` file reads traced back from a load call) and the configured SQL
    // dialect (for identifier case folding — see `sql::SqlDialect`). Kept as a
    // post-construction setter rather than a `new()` parameter so the 35+ existing
    // `Linter::new()` call sites (mostly tests, which don't care about either) are
    // undisturbed.
    pub fn with_context(&mut self, root: PathBuf, config: &LinterConfig) {
        self.project_root = Some(root);
        if let Some(dialect) = &config.sql_dialect {
            self.sql_dialect = sql::SqlDialect::from_config_str(dialect);
        }
    }

    // Convert a byte offset to a 1-indexed (line, column) pair using the pre-built
    // LineIndex (O(log n) binary search).  Values come from OneIndexed::get() so they
    // are already 1-based — no adjustment needed at call sites.
    fn source_location(&self, offset: ruff_text_size::TextSize) -> (usize, usize) {
        let source_code = SourceCode::new(
            &self.source,
            self.line_index
                .as_ref()
                .expect("LineIndex should be initialized before calling source_location"),
        );
        let loc = source_code.line_column(offset);
        (loc.line.get(), loc.column.get())
    }

    // Format a schema name for use in an error message.
    //
    // For inferred schemas (those whose name starts with `__inferred_`):
    //   - includes the full column set so the user can see what IS available
    //   - includes the origin function/file when recorded by load_cross_file_symbols
    //   - reports the line in the current file where the variable was bound
    //
    // For named schemas (BaseSchema subclasses): returns the schema name + defined line.
    fn schema_display(&self, schema_name: &str, defined_line: usize) -> String {
        if schema_name.starts_with("__inferred_") {
            let cols = self.schemas.get(schema_name).cloned().unwrap_or_default();
            let cols_str = cols.join(", ");
            if let Some(origin) = self.schema_origins.get(schema_name) {
                format!(
                    "inferred column set {{{cols_str}}} — fix: add the column at its source in {origin}"
                )
            } else {
                format!("inferred column set {{{cols_str}}} (defined at line {defined_line})")
            }
        } else {
            format!("{schema_name} (defined at line {defined_line})")
        }
    }

    // Parse `source`, walk the AST, then filter out any diagnostic whose line carries a
    // `# typedframes: ignore` comment.  Returns the surviving errors.
    pub fn check_file_internal(
        &mut self,
        source: &str,
        path: &Path,
    ) -> Result<Vec<LintError>, anyhow::Error> {
        self.source = source.to_string();
        self.file_display = path.display().to_string();
        self.line_index = Some(LineIndex::from_source_text(source));
        let parsed = parse_module(source).map_err(|e| anyhow::anyhow!("{e}"))?;
        let module = parsed.into_syntax();
        self.string_var_candidates = self.collect_string_var_candidates(&module.body, path);
        let mut errors = Vec::new();

        for stmt in &module.body {
            self.visit_stmt(stmt, &mut errors);
        }

        errors.retain(|e| !is_line_ignored(source, e.line, &e.code));

        Ok(errors)
    }

    // Load schemas and functions from cross-file index based on import statements.
    fn load_cross_file_symbols(
        &mut self,
        index: &ProjectIndex,
        source: &str,
        _file_path: &Path,
        project_root: &Path,
    ) {
        // A function's return-type schema (or a parameter's schema annotation) may be
        // defined in a THIRD file — neither the function's own file nor the file
        // importing the function. E.g. schemas.py defines CustomerSchema, loaders.py's
        // load_customers() returns Annotated[pd.DataFrame, CustomerSchema], and
        // pipeline.py imports only load_customers, not CustomerSchema directly. Looking
        // the schema up only in the function's own file's entry (as opposed to
        // project-wide) would silently fail in that case — no error, but no validation
        // either, since self.schemas would never learn CustomerSchema's columns.
        // `index.all_schemas` is this project-wide name -> columns registry — computed
        // ONCE in build_index_internal, not rebuilt here. This function runs once per
        // file checked (see `check_file`), so rebuilding a project-wide map in here
        // would cost O(files) per file, i.e. O(files^2) for a whole-project check.
        let all_schemas = &index.all_schemas;

        let Ok(parsed) = parse_module(source) else {
            return;
        };
        let module = parsed.into_syntax();
        for stmt in &module.body {
            let Stmt::ImportFrom(import_from) = stmt else {
                continue;
            };
            if import_from.level > 0 {
                continue;
            }
            let Some(module_ident) = &import_from.module else {
                continue;
            };
            let module_name = module_ident.id.as_str();
            if module_name.starts_with("typedframes") {
                continue;
            }
            // resolve_module_file also tries an explicitly allowlisted external
            // package's site-packages location (see collect_external_package_files),
            // not just project_root/project_root/src.
            let Some(resolved_str) = resolve_module_file(module_name, project_root, &index.files)
            else {
                continue;
            };
            let Some(entry) = index.files.get(&resolved_str) else {
                continue;
            };
            // Use the full resolved path (not just the basename) so error messages
            // contain an openable `file:line` reference regardless of cwd.
            let file_path_display = resolved_str.clone();
            // `from X import *`: ruff represents the wildcard as a single alias named
            // "*". Expand to the module's declared __all__, or — matching real Python
            // semantics for a module with no __all__ — every public (non-`_`-prefixed)
            // function/schema name, since we can't tell which of those the file
            // actually goes on to use.
            let is_wildcard =
                import_from.names.len() == 1 && import_from.names[0].name.as_str() == "*";
            if is_wildcard {
                let names: Vec<String> = if !entry.exports.is_empty() {
                    entry.exports.clone()
                } else {
                    entry
                        .functions
                        .keys()
                        .chain(entry.schemas.keys())
                        .filter(|n| !n.starts_with('_'))
                        .cloned()
                        .collect()
                };
                for name in &names {
                    self.import_name(entry, name, &file_path_display, all_schemas);
                }
                continue;
            }
            for alias in &import_from.names {
                let name = alias.name.id.as_str();
                self.import_name(entry, name, &file_path_display, all_schemas);
            }
        }

        // Plain `import module [as alias]`: everything reachable via `module.name`
        // attribute access. Unlike `from X import name`, there's no explicit name
        // list to narrow to, so pull in every function this module defines — a call
        // site written as `module.func(df)` is resolved by check_call_requirements
        // looking up the bare name `func`, the same key space `from X import func`
        // would have populated (see visit_stmt's Expr::Attribute call-site handling).
        for stmt in &module.body {
            let Stmt::Import(import_stmt) = stmt else {
                continue;
            };
            for alias in &import_stmt.names {
                let dotted = alias.name.id.as_str();
                if dotted.starts_with("typedframes") {
                    continue;
                }
                let Some(resolved_path) =
                    resolve_module_file(dotted, project_root, &index.files).map(PathBuf::from)
                else {
                    continue;
                };
                let Some(resolved_str) = resolved_path.to_str() else {
                    continue;
                };
                let Some(entry) = index.files.get(resolved_str) else {
                    continue;
                };
                let file_path_display = resolved_path.display().to_string();
                let names: Vec<String> = entry.functions.keys().cloned().collect();
                for name in &names {
                    self.import_name(entry, name, &file_path_display, all_schemas);
                }
            }
        }
    }

    // Pull a single name's schema/function/param-contract info from a resolved
    // cross-file IndexEntry into this Linter's own local state. Shared by
    // `from X import name`, `from X import *`, and plain `import module` handling
    // in load_cross_file_symbols — all three ultimately need to do the same thing
    // for each name they bring into scope.
    fn import_name(
        &mut self,
        entry: &IndexEntry,
        name: &str,
        file_path_display: &str,
        all_schemas: &HashMap<String, Vec<String>>,
    ) {
        if let Some(cols) = entry.schemas.get(name) {
            self.schemas.insert(name.to_string(), cols.clone());
        }
        let Some(func) = entry.functions.get(name) else {
            return;
        };
        if !func.returns_schema.is_empty() {
            self.functions
                .insert(name.to_string(), func.returns_schema.clone());
        }
        if let Some(schema_cols) = all_schemas.get(func.returns_schema.as_str()) {
            self.schemas
                .insert(func.returns_schema.clone(), schema_cols.clone());
            // Record origin so error messages point back to the source function/file.
            // The inferred schema name encodes the definition line as
            // `__inferred_<var>_at_<line>`, so we can surface the exact
            // location without storing extra metadata.
            if func.returns_schema.starts_with("__inferred_") {
                let line_suffix = func
                    .returns_schema
                    .strip_prefix("__inferred_")
                    .and_then(|s| s.rsplit_once("_at_"))
                    .and_then(|(_, l)| l.parse::<usize>().ok())
                    .map(|l| format!(":{l}"))
                    .unwrap_or_default();
                self.schema_origins.insert(
                    func.returns_schema.clone(),
                    format!("{name} ({file_path_display}{line_suffix})"),
                );
            }
        }
        // Record the parameter-column contract so calls to this function
        // (e.g. `trim(customers)`) can be validated at the call site,
        // where the actual argument's inferred schema is known.
        if !func.requires.is_empty() {
            let origin = format!("{name} ({file_path_display}:{})", func.def_line);
            self.param_requires
                .insert(name.to_string(), (func.requires.clone(), origin));
        }
    }

    // Check if a base class name indicates a typedframes schema
    fn is_schema_base(name: &str) -> bool {
        matches!(
            name,
            "BaseSchema" | "DataFrameModel" | "DataFrame" | "BaseFrame"
        )
    }

    fn extract_string_literal(expr: &Expr) -> Option<&str> {
        if let Expr::StringLiteral(s) = expr {
            Some(s.value.to_str())
        } else {
            None
        }
    }

    // `__tablename__ = "orders"` (or `__tablename__: str = "orders"`) in a class's own
    // body — the structural signature of a SQLAlchemy declarative model, checked
    // instead of base-class name since the declarative base is normally imported from a
    // project-local module. Deliberately does not walk inherited/mixin base classes for
    // this — an abstract mixin that itself has no `__tablename__` isn't recognized.
    fn class_body_has_tablename(class_def: &ast::StmtClassDef) -> bool {
        class_def.body.iter().any(|stmt| {
            let (target, value) = match stmt {
                Stmt::Assign(assign) => match assign.targets.as_slice() {
                    [Expr::Name(n)] => (n.id.as_str(), Some(assign.value.as_ref())),
                    _ => return false,
                },
                Stmt::AnnAssign(ann) => match ann.target.as_ref() {
                    Expr::Name(n) => (n.id.as_str(), ann.value.as_deref()),
                    _ => return false,
                },
                _ => return false,
            };
            target == "__tablename__" && value.and_then(Self::extract_string_literal).is_some()
        })
    }

    // Class attribute names that never denote a mapped column, whatever they're
    // assigned to.
    const ORM_NON_COLUMN_ATTRS: &[&str] = &[
        "__tablename__",
        "__table_args__",
        "__mapper_args__",
        "__table__",
        "metadata",
        "registry",
        "query",
    ];

    // Calls that construct a legitimate class attribute with no corresponding database
    // column — excluded rather than swept in as a column the way the permissive
    // typedframes-schema extractor's fallback (used for `is_schema_base` classes) would.
    const ORM_NON_COLUMN_CALLS: &[&str] = &[
        "relationship",
        "column_property",
        "association_proxy",
        "declared_attr",
        "query_expression",
        "synonym",
    ];

    // Column extractor for a SQLAlchemy declarative model (see `class_body_has_tablename`).
    // Deliberately separate from the typedframes-schema extractor above rather than
    // sharing it: that extractor's fallback treats *any* annotated or assigned class
    // attribute as a column, which on a real model would sweep in `__tablename__`,
    // `__table_args__`, `relationship(...)` attributes, and `Mapped[list[...]]`
    // to-many-relationship annotations. This one uses an allowlist instead: an
    // attribute is a column only if it's a `Column(...)`/`mapped_column(...)` call
    // (optionally wrapped in `deferred(...)`), or a bare `x: Mapped[T]` with no value
    // where `T` isn't itself a relationship shape.
    fn extract_orm_columns(class_def: &ast::StmtClassDef) -> Vec<String> {
        let mut columns = Vec::new();
        for body_stmt in &class_def.body {
            match body_stmt {
                Stmt::AnnAssign(ann) => {
                    let Expr::Name(name) = ann.target.as_ref() else {
                        continue;
                    };
                    let attr_name = name.id.as_str();
                    if attr_name.starts_with('_') || Self::ORM_NON_COLUMN_ATTRS.contains(&attr_name)
                    {
                        continue;
                    }
                    match &ann.value {
                        Some(value) => {
                            if let Some(cols) = Self::orm_column_from_call(value, attr_name) {
                                columns.extend(cols);
                            }
                        }
                        None => {
                            if !Self::is_relationship_annotation(&ann.annotation) {
                                columns.push(attr_name.to_string());
                            }
                        }
                    }
                }
                Stmt::Assign(assign) => {
                    let [Expr::Name(name)] = assign.targets.as_slice() else {
                        continue;
                    };
                    let attr_name = name.id.as_str();
                    if attr_name.starts_with('_') || Self::ORM_NON_COLUMN_ATTRS.contains(&attr_name)
                    {
                        continue;
                    }
                    if let Some(cols) = Self::orm_column_from_call(&assign.value, attr_name) {
                        columns.extend(cols);
                    }
                }
                _ => {}
            }
        }
        columns.sort();
        columns.dedup();
        columns
    }

    // `Column(...)` / `mapped_column(...)` / `deferred(Column(...))` → the resulting
    // column name(s): the attribute name, plus a DB-name override from a leading
    // positional string literal or a `name=` keyword when it differs from the
    // attribute name — SQLAlchemy allows code to reference either spelling
    // (`mapped_column("db_name", ...)` puts the real DB name in the first positional
    // arg), so registering both avoids a spurious unknown-column on whichever one a
    // caller writes. `relationship(...)` and friends (`ORM_NON_COLUMN_CALLS`) return
    // `None` — excluded rather than swept in, unlike the permissive fallback the
    // non-ORM schema extractor above uses.
    fn orm_column_from_call(value: &Expr, attr_name: &str) -> Option<Vec<String>> {
        let Expr::Call(call) = value else {
            return None;
        };
        let fn_name = match &*call.func {
            Expr::Name(n) => n.id.as_str(),
            Expr::Attribute(a) => a.attr.as_str(),
            _ => return None,
        };
        if fn_name == "deferred" {
            return call
                .arguments
                .args
                .first()
                .and_then(|inner| Self::orm_column_from_call(inner, attr_name));
        }
        if Self::ORM_NON_COLUMN_CALLS.contains(&fn_name) {
            return None;
        }
        if fn_name != "Column" && fn_name != "mapped_column" {
            return None;
        }

        let mut names = vec![attr_name.to_string()];
        if let Some(db_name) = call
            .arguments
            .args
            .first()
            .and_then(Self::extract_string_literal)
        {
            if db_name != attr_name {
                names.push(db_name.to_string());
            }
        }
        for keyword in &call.arguments.keywords {
            if keyword.arg.as_ref().map(|s| s.as_str()) == Some("name") {
                if let Some(db_name) = Self::extract_string_literal(&keyword.value) {
                    if db_name != attr_name && !names.iter().any(|n| n == db_name) {
                        names.push(db_name.to_string());
                    }
                }
            }
        }
        Some(names)
    }

    // `Mapped[list[...]]` / `Mapped[List[...]]` / `Mapped[Set[...]]` (a to-many
    // relationship's typing shape) or `Mapped["OtherModel"]` (a quoted forward
    // reference — the idiom for a to-one relationship typed without importing the
    // referenced class) — never a real column, whatever the bare-annotation fallback
    // in `extract_orm_columns` would otherwise assume.
    fn is_relationship_annotation(annotation: &Expr) -> bool {
        let Expr::Subscript(sub) = annotation else {
            return false;
        };
        let is_mapped = match &*sub.value {
            Expr::Name(n) => n.id.as_str() == "Mapped",
            Expr::Attribute(a) => a.attr.as_str() == "Mapped",
            _ => false,
        };
        if !is_mapped {
            return false;
        }
        match &*sub.slice {
            Expr::StringLiteral(_) => true,
            Expr::Subscript(inner) => {
                let collection_name = match &*inner.value {
                    Expr::Name(n) => Some(n.id.as_str()),
                    Expr::Attribute(a) => Some(a.attr.as_str()),
                    _ => None,
                };
                matches!(
                    collection_name,
                    Some("list" | "List" | "set" | "Set" | "Sequence")
                )
            }
            _ => false,
        }
    }

    // Check if a type name is a DataFrame/Frame type
    fn is_frame_type(name: &str) -> bool {
        matches!(name, "DataFrame" | "PandasFrame" | "PolarsFrame")
    }

    // Extract schema name from a type annotation like PandasFrame[Schema] or Annotated[pd.DataFrame, Schema]
    fn extract_schema_from_annotation(expr: &Expr) -> Option<&str> {
        match expr {
            Expr::Subscript(subscript) => {
                let type_name = match &*subscript.value {
                    Expr::Name(name) => Some(name.id.as_str()),
                    Expr::Attribute(attr) => Some(attr.attr.as_str()),
                    _ => None,
                };
                if let Some(name) = type_name {
                    if Self::is_frame_type(name) {
                        if let Expr::Name(schema_name) = &*subscript.slice {
                            return Some(schema_name.id.as_str());
                        }
                    }
                    // Handle Annotated[pd.DataFrame, Schema] — schema is second tuple element
                    if name == "Annotated" {
                        if let Expr::Tuple(tuple) = &*subscript.slice {
                            if tuple.elts.len() >= 2 {
                                if let Expr::Name(schema_name) = &tuple.elts[1] {
                                    return Some(schema_name.id.as_str());
                                }
                            }
                        }
                    }
                }
                None
            }
            Expr::StringLiteral(s) => {
                let text = s.value.to_str();
                let patterns = ["DataFrame[", "PandasFrame[", "PolarsFrame["];
                for pattern in patterns {
                    if text.contains(pattern) {
                        if let Some(start) = text.find('[') {
                            if let Some(end) = text.rfind(']') {
                                let schema = text[start + 1..end].trim();
                                if !schema.is_empty() && !schema.contains(',') {
                                    return Some(schema);
                                }
                            }
                        }
                    }
                }
                None
            }
            _ => None,
        }
    }

    // Extract a list of string literals from a `["a", "b", ...]` list expression.
    // Returns None if the expression is not a list or any element is not a string literal.
    fn extract_string_list(expr: &Expr) -> Option<Vec<String>> {
        if let Expr::List(list) = expr {
            let mut result = Vec::new();
            for el in &list.elts {
                if let Expr::StringLiteral(s) = el {
                    result.push(s.value.to_str().to_string());
                } else {
                    return None;
                }
            }
            Some(result)
        } else {
            None
        }
    }

    // Extract columns from a list or single string expression.
    fn extract_string_list_or_single(expr: &Expr) -> Option<Vec<String>> {
        match expr {
            Expr::List(_) => Self::extract_string_list(expr),
            Expr::StringLiteral(s) => Some(vec![s.value.to_str().to_string()]),
            _ => None,
        }
    }

    // Extract column names from a load function call: `usecols`/`columns` kwarg,
    // `dtype`/`schema` dict keys, or — for SQL-shaped load functions — the SELECT list
    // of a literal SQL string. Returns the columns alongside which family satisfied the
    // extraction, so the caller can phrase a kind-appropriate `untracked-dataframe` hint
    // when extraction fails.
    fn extract_load_columns(
        &self,
        func_name: &str,
        call: &ast::ExprCall,
    ) -> (Option<Vec<String>>, LoadKind) {
        for keyword in &call.arguments.keywords {
            let kw_name = keyword.arg.as_ref().map(|s| s.as_str());
            match kw_name {
                Some("usecols") | Some("columns") => {
                    if let Some(cols) = Self::extract_string_list(&keyword.value) {
                        return (Some(cols), LoadKind::File);
                    }
                }
                Some("dtype") | Some("schema") => {
                    if let Expr::Dict(dict) = &keyword.value {
                        let keys: Vec<String> = dict
                            .items
                            .iter()
                            .filter_map(|item| item.key.as_ref())
                            .filter_map(|k| Self::extract_string_literal(k))
                            .map(|s| s.to_string())
                            .collect();
                        if !keys.is_empty() {
                            return (Some(keys), LoadKind::File);
                        }
                    }
                }
                _ => {}
            }
        }

        if !SQL_LOAD_FUNCTIONS.contains(&func_name) {
            return (None, LoadKind::File);
        }

        // chunksize= makes the call return an iterator of frames, not a single frame —
        // there is no one column set to attach to the assigned variable.
        let has_chunksize = call
            .arguments
            .keywords
            .iter()
            .any(|k| k.arg.as_ref().map(|s| s.as_str()) == Some("chunksize"));
        if has_chunksize {
            return (None, LoadKind::Sql);
        }

        // A SQLAlchemy Core statement (`pd.read_sql(select(Order.id, ...), engine)`, or
        // a `Name` bound to one) takes priority over SQL-text parsing — it isn't a
        // string at all, so `extract_sql_literal` would never match it anyway, but
        // checking first keeps the two paths clearly separate rather than relying on
        // that fallthrough.
        if let Some(cols) = self.extract_orm_select_columns(call) {
            return (Some(cols), LoadKind::Orm);
        }

        let Some(sql_text) = self.extract_sql_literal(call) else {
            return (None, LoadKind::Sql);
        };

        match sql::columns_from_select(&sql_text, self.sql_dialect) {
            sql::SqlOutcome::Columns(cols) => (Some(cols), LoadKind::Sql),
            sql::SqlOutcome::Wildcard | sql::SqlOutcome::Unparsed => (None, LoadKind::Sql),
        }
    }

    // Locate a literal SQL string passed to a SQL-shaped load call: the `sql=`/`query=`
    // keyword if present, else the first positional argument. Unwraps one layer of
    // `text(...)` / `sqlalchemy.text(...)`, the idiomatic way to pass raw SQL through a
    // SQLAlchemy engine. Returns `None` for f-strings, variables, or anything else that
    // isn't a literal — those cases fall through to the existing untracked-dataframe
    // nudge rather than being (wrongly) treated as unresolvable SQL.
    fn extract_sql_literal(&self, call: &ast::ExprCall) -> Option<String> {
        for keyword in &call.arguments.keywords {
            if matches!(
                keyword.arg.as_ref().map(|s| s.as_str()),
                Some("sql") | Some("query")
            ) {
                return self.extract_sql_expr(&keyword.value);
            }
        }
        call.arguments
            .args
            .first()
            .and_then(|expr| self.extract_sql_expr(expr))
    }

    // Resolve a SQL-shaped argument expression to its text: a literal string, a
    // `text(...)`/`sqlalchemy.text(...)`-wrapped literal, or a `Name` bound (per
    // `string_var_candidates`) to a literal or a resolvable `.sql` file read. Deliberately
    // does NOT resolve f-strings, `.format()`, string concatenation, or any variable
    // reassigned more than once — see `StringBindingCollector` for why, and
    // `check_file_internal`'s `LoadKind::Sql` untracked-dataframe hint for how that's
    // surfaced to the user instead of silently guessing.
    fn extract_sql_expr(&self, expr: &Expr) -> Option<String> {
        if let Some(s) = Self::extract_string_literal(expr) {
            return Some(s.to_string());
        }
        if let Expr::Name(name) = expr {
            return self.string_var_candidates.get(name.id.as_str()).cloned();
        }
        if let Expr::Call(inner) = expr {
            let fn_name = match &*inner.func {
                Expr::Name(n) => n.id.as_str(),
                Expr::Attribute(a) => a.attr.as_str(),
                _ => return None,
            };
            if fn_name == "text" {
                return inner
                    .arguments
                    .args
                    .first()
                    .and_then(|expr| self.extract_sql_expr(expr));
            }
        }
        None
    }

    // Column-preserving Core statement methods: chaining any of these onto a
    // `select(...)` doesn't change which columns are projected, so `extract_select_columns`
    // sees through them to the underlying `select(...)` call. Anything else chained
    // (`.subquery()`, `.cte()`, `.union(...)`, `.add_columns(...)`, ...) isn't
    // recognized, and the whole expression is left unresolved rather than guessed at.
    const SELECT_CHAIN_METHODS: &[&str] = &[
        "where", "filter", "join", "order_by", "limit", "offset", "group_by", "having", "distinct",
    ];

    // Locate the SQL-shaped argument (`sql=`/`query=` keyword, else first positional)
    // the same way `extract_sql_literal` does, but resolve it as a SQLAlchemy Core
    // statement's column list instead of as SQL text.
    fn extract_orm_select_columns(&self, call: &ast::ExprCall) -> Option<Vec<String>> {
        for keyword in &call.arguments.keywords {
            if matches!(
                keyword.arg.as_ref().map(|s| s.as_str()),
                Some("sql") | Some("query")
            ) {
                return self.extract_select_columns(&keyword.value);
            }
        }
        call.arguments
            .args
            .first()
            .and_then(|expr| self.extract_select_columns(expr))
    }

    // Resolve a SQLAlchemy Core statement expression to its projected column list: a
    // `select(...)` call, a chain of `SELECT_CHAIN_METHODS` on one, or a `Name` bound
    // (via `stmt_var_candidates`) to an already-resolved one.
    fn extract_select_columns(&self, expr: &Expr) -> Option<Vec<String>> {
        match expr {
            Expr::Name(n) => self.stmt_var_candidates.get(n.id.as_str()).cloned(),
            Expr::Call(call) => {
                let fn_name = match &*call.func {
                    Expr::Name(n) => n.id.as_str(),
                    Expr::Attribute(a) => a.attr.as_str(),
                    _ => return None,
                };
                if fn_name == "select" {
                    return self.extract_select_args(&call.arguments.args);
                }
                if Self::SELECT_CHAIN_METHODS.contains(&fn_name) {
                    if let Expr::Attribute(attr) = &*call.func {
                        return self.extract_select_columns(&attr.value);
                    }
                }
                None
            }
            _ => None,
        }
    }

    // The column list for `select(arg1, arg2, ...)`: every argument must resolve to a
    // single, unambiguous column name (see `extract_select_arg_column`), or the whole
    // select is left unresolved — never a partial list, since a silently-dropped
    // argument would understate the real projection and risk a false unknown-column
    // later on a column that's actually there. In particular a bare `select(Model)`
    // (all of a model's columns) is deliberately NOT supported: `extract_orm_columns`
    // is allowlist-based and can under-extract on an unusual declarative pattern, and
    // treating its output as "the complete column set" here would compound that.
    fn extract_select_args(&self, args: &[Expr]) -> Option<Vec<String>> {
        if args.is_empty() {
            return None;
        }
        args.iter()
            .map(|arg| self.extract_select_arg_column(arg))
            .collect()
    }

    // A single `select(...)` argument's resulting column name: `Model.col` (an
    // attribute referencing a registered model's known column), or
    // `Model.col.label("alias")`. Anything else — a bare model, `func.count(...)`,
    // `text(...)`, a literal, a starred arg — bails the whole select rather than
    // guessing.
    fn extract_select_arg_column(&self, arg: &Expr) -> Option<String> {
        if let Expr::Call(call) = arg {
            if let Expr::Attribute(outer) = &*call.func {
                if outer.attr.as_str() == "label" {
                    self.validate_model_attribute(&outer.value)?;
                    let alias = call
                        .arguments
                        .args
                        .first()
                        .and_then(Self::extract_string_literal)?;
                    return Some(alias.to_string());
                }
            }
            return None;
        }
        self.validate_model_attribute(arg)
    }

    // `Model.col` where `Model` is a registered schema (declarative model or otherwise)
    // and `col` is one of its known columns. Returns `None` — bailing the containing
    // select entirely — if `Model` isn't registered or `col` isn't one of its columns:
    // hybrid properties, synonyms, and association proxies are all legal SQLAlchemy
    // attributes with no static column mapping, so treating an unrecognized attribute
    // as an error would be wrong at least as often as it would be right.
    fn validate_model_attribute(&self, expr: &Expr) -> Option<String> {
        let Expr::Attribute(attr) = expr else {
            return None;
        };
        let Expr::Name(model) = &*attr.value else {
            return None;
        };
        let columns = self.schemas.get(model.id.as_str())?;
        let col = attr.attr.as_str();
        columns.iter().any(|c| c == col).then(|| col.to_string())
    }

    // Column set for a Feast `features=["view:feature", ...]` list: each element must
    // be a `"view:feature"` string literal (the ref format Feast documents), and the
    // resulting column is the part after the colon — or, when `full_feature_names=True`,
    // `"view__feature"` (double underscore, per Feast's own docstring). Bails entirely
    // (`None`) on: no `features=` keyword at all (so a plain method-name match alone
    // never dispatches — see `FEAST_RETRIEVAL_METHODS`'s call sites), a non-literal
    // list element (a `Name`, a comprehension, a `FeatureService` member projection
    // like `fv[["conv_rate"]]`), an element without exactly one `:`, or a non-literal
    // `full_feature_names`. Never a partial column set for the same reason as
    // `extract_select_args`: a silently-dropped element would understate the real
    // projection.
    //
    // Note this can NEVER be the complete output column set regardless — Feast's real
    // output also includes entity_df's join keys and timestamp column, which aren't
    // resolvable in general. That's handled at the call site by registering the result
    // as an *open* schema (`register_feast_dataframe`), not by trying to enumerate
    // those columns here.
    fn extract_feast_feature_columns(&self, call: &ast::ExprCall) -> Option<Vec<String>> {
        let features_list = call
            .arguments
            .keywords
            .iter()
            .find(|k| k.arg.as_ref().map(|s| s.as_str()) == Some("features"))
            .map(|k| &k.value)?;
        let Expr::List(list) = features_list else {
            return None;
        };

        let full_feature_names = match call
            .arguments
            .keywords
            .iter()
            .find(|k| k.arg.as_ref().map(|s| s.as_str()) == Some("full_feature_names"))
        {
            Some(kw) => match &kw.value {
                Expr::BooleanLiteral(b) => b.value,
                _ => return None,
            },
            None => false,
        };

        let mut columns = Vec::with_capacity(list.elts.len());
        for elt in &list.elts {
            let literal = Self::extract_string_literal(elt)?;
            let (view, feature) = literal.split_once(':')?;
            // `feature` still contains any colon after the first (split_once only
            // splits on the first match), so this rejects anything but exactly one `:`
            // in the whole ref.
            if view.is_empty() || feature.is_empty() || feature.contains(':') {
                return None;
            }
            columns.push(if full_feature_names {
                format!("{view}__{feature}")
            } else {
                feature.to_string()
            });
        }
        Some(columns)
    }

    // Register `target_names` as a DataFrame materialized from a Feast retrieval
    // (`.to_df()` on a `get_historical_features`/`get_online_features` result), with an
    // *open* schema over `cols` when resolved. See `open_schemas`'s docs for why exact
    // matching would be wrong here regardless of how well `features=` parsed.
    fn register_feast_dataframe(
        &mut self,
        cols: Option<Vec<String>>,
        target_names: &[String],
        var_hint: &str,
        current_line: usize,
        current_col: usize,
        errors: &mut Vec<LintError>,
    ) {
        self.dataframes_total += 1;
        match cols {
            Some(cols) => {
                self.dataframes_typed += 1;
                let schema_name = self.make_inferred_schema(cols, var_hint, current_line);
                self.open_schemas.insert(schema_name.clone());
                for name in target_names {
                    self.variables
                        .insert(name.clone(), (schema_name.clone(), current_line));
                }
            }
            None => {
                errors.push(LintError {
                    line: current_line,
                    col: current_col,
                    code: CODE_UNTRACKED_DATAFRAME.to_string(),
                    message: "columns unknown at lint time; pass a literal \
                              `features=[\"view:feature\", ...]` list to resolve the \
                              retrieved columns"
                        .to_string(),
                    severity: "warning".to_string(),
                });
            }
        }
    }

    // If `expr` is a call to one of `SQL_PRODUCING_METHODS` (`client.query(...)`,
    // `spark.sql(...)`, `duckdb.sql(...)`/`duckdb.query(...)`), return it so its SQL
    // argument can be resolved — otherwise `None`. Kept as a separate check (rather than
    // folding straight into column extraction) so the chained-finalize dispatch below
    // can tell "not our pattern at all" (stay silent) apart from "our pattern, but the
    // SQL didn't resolve" (worth an untracked-dataframe hint) — see its call site.
    fn sql_producing_call(expr: &Expr) -> Option<&ast::ExprCall> {
        let Expr::Call(call) = expr else {
            return None;
        };
        let fn_name = match &*call.func {
            Expr::Name(n) => n.id.as_str(),
            Expr::Attribute(a) => a.attr.as_str(),
            _ => return None,
        };
        SQL_PRODUCING_METHODS.contains(&fn_name).then_some(call)
    }

    // Register `target_names` as a DataFrame with an ordinary (exact-match) schema
    // inferred from a literal SQL string, or an untracked-dataframe warning if `sql`
    // couldn't be resolved to one. Shared by the chained-finalize
    // (`client.query(sql).to_dataframe()`) and cursor (`cursor.fetch_pandas_all()`)
    // connector patterns, which differ only in how they locate the SQL text.
    fn register_sql_dataframe(
        &mut self,
        sql: Option<&str>,
        target_names: &[String],
        var_hint: &str,
        current_line: usize,
        current_col: usize,
        errors: &mut Vec<LintError>,
    ) {
        self.dataframes_total += 1;
        let cols = sql.and_then(
            |sql| match sql::columns_from_select(sql, self.sql_dialect) {
                sql::SqlOutcome::Columns(cols) => Some(cols),
                sql::SqlOutcome::Wildcard | sql::SqlOutcome::Unparsed => None,
            },
        );
        match cols {
            Some(cols) => {
                self.dataframes_typed += 1;
                let schema_name = self.make_inferred_schema(cols, var_hint, current_line);
                for name in target_names {
                    self.variables
                        .insert(name.clone(), (schema_name.clone(), current_line));
                }
            }
            None => {
                errors.push(LintError {
                    line: current_line,
                    col: current_col,
                    code: CODE_UNTRACKED_DATAFRAME.to_string(),
                    message: "columns unknown at lint time; name the columns in the \
                              `SELECT` list (avoid `SELECT *`) or annotate: \
                              `df: Annotated[pd.DataFrame, MySchema] = ...`"
                        .to_string(),
                    severity: "warning".to_string(),
                });
            }
        }
    }

    // Resolve the right-hand side of a candidate string-variable binding: a plain
    // string literal, or a recognized `.sql` file read (`open(p).read()`,
    // `Path(p).read_text()`/`pathlib.Path(p).read_text()`). Called only from
    // `StringBindingCollector` while building `string_var_candidates` — NOT from
    // `extract_sql_expr` directly, since by the time a load call is checked, any file
    // read has already been resolved once (and budget-capped) during the pre-pass.
    fn resolve_literal_rhs(
        &self,
        expr: &Expr,
        current_file: &Path,
        reads_used: &mut u32,
    ) -> Option<String> {
        if let Some(s) = Self::extract_string_literal(expr) {
            return Some(s.to_string());
        }
        if let Expr::Call(call) = expr {
            return self.resolve_file_read_call(call, current_file, reads_used);
        }
        None
    }

    // Match `open(p).read()` and `p.read_text()` (where `p` is itself a path
    // expression — `Path(p)`/`pathlib.Path(p)`, or `Path(__file__).parent / "x.sql"`
    // with no further `Path(...)` wrapper, since nobody writes
    // `Path(Path(__file__).parent / "x.sql")`), resolve the path via
    // `resolve_path_expr`, and read the file through `read_sql_file`'s safety checks.
    // The split `with open(p) as f: ... sql = f.read()` form is deliberately not
    // handled — it would need a second traced-binding namespace for file handles, and
    // the direct chained form covers the common case.
    fn resolve_file_read_call(
        &self,
        call: &ast::ExprCall,
        current_file: &Path,
        reads_used: &mut u32,
    ) -> Option<String> {
        let Expr::Attribute(attr) = &*call.func else {
            return None;
        };

        match attr.attr.as_str() {
            "read" => {
                // open(p).read()
                let Expr::Call(inner) = &*attr.value else {
                    return None;
                };
                let is_open = matches!(&*inner.func, Expr::Name(n) if n.id.as_str() == "open");
                if !is_open {
                    return None;
                }
                // Reject a binary-mode open ("rb" etc.) rather than reading raw bytes
                // as if they were UTF-8 SQL text.
                let binary_mode = inner
                    .arguments
                    .args
                    .iter()
                    .chain(inner.arguments.keywords.iter().map(|k| &k.value))
                    .any(|a| matches!(Self::extract_string_literal(a), Some(m) if m.contains('b')));
                if binary_mode {
                    return None;
                }
                let path_arg = inner.arguments.args.first()?;
                let path = self.resolve_path_expr(path_arg, current_file)?;
                self.read_sql_file(&path, reads_used)
            }
            "read_text" => {
                // The receiver of `.read_text()` IS the path expression itself —
                // `Path(p).read_text()` or `(Path(__file__).parent / "x.sql").read_text()`.
                let path = self.resolve_path_expr(&attr.value, current_file)?;
                self.read_sql_file(&path, reads_used)
            }
            _ => None,
        }
    }

    // Resolve a path-shaped expression to a filesystem path: a `Path(p)`/
    // `pathlib.Path(p)` call over a string literal, or
    // `Path(__file__).parent / "literal.sql"` anchored to the file currently being
    // checked. Deliberately does NOT resolve a `Name` through `string_var_candidates` —
    // path variables and SQL-text variables share no ordering guarantee within a single
    // linear pre-pass, so doing that safely would need a fixed-point resolution loop;
    // out of scope while call sites overwhelmingly pass the path inline.
    fn resolve_path_expr(&self, expr: &Expr, current_file: &Path) -> Option<PathBuf> {
        if let Some(s) = Self::extract_string_literal(expr) {
            return Some(PathBuf::from(s));
        }
        if let Expr::Call(call) = expr {
            let is_path_ctor = match &*call.func {
                Expr::Name(n) => n.id.as_str() == "Path",
                Expr::Attribute(a) => a.attr.as_str() == "Path",
                _ => false,
            };
            if is_path_ctor {
                return call
                    .arguments
                    .args
                    .first()
                    .and_then(Self::extract_string_literal)
                    .map(PathBuf::from);
            }
        }
        if let Expr::BinOp(binop) = expr {
            if binop.op == ast::Operator::Div && Self::is_file_parent_expr(&binop.left) {
                if let Some(s) = Self::extract_string_literal(&binop.right) {
                    return current_file.parent().map(|dir| dir.join(s));
                }
            }
        }
        None
    }

    // Matches `Path(__file__).parent` (with or without a `pathlib.` prefix on `Path`).
    fn is_file_parent_expr(expr: &Expr) -> bool {
        let Expr::Attribute(attr) = expr else {
            return false;
        };
        if attr.attr.as_str() != "parent" {
            return false;
        }
        let Expr::Call(call) = &*attr.value else {
            return false;
        };
        let is_path_ctor = match &*call.func {
            Expr::Name(n) => n.id.as_str() == "Path",
            Expr::Attribute(a) => a.attr.as_str() == "Path",
            _ => false,
        };
        is_path_ctor
            && matches!(
                call.arguments.args.first(),
                Some(Expr::Name(n)) if n.id.as_str() == "__file__"
            )
    }

    // Safety-checked read of a `.sql` file traced back from a load call. The real
    // security boundary is project-root containment, checked below AFTER
    // canonicalizing both sides (which also catches symlink escapes, since
    // canonicalization follows symlinks) — NOT rejecting absolute paths outright, since
    // `resolve_path_arg`'s `Path(__file__).parent / "x.sql"` case legitimately produces
    // an absolute path whenever the file being checked was itself passed in as an
    // absolute path (the normal case for every real caller). An absolute path a user
    // wrote directly, e.g. `Path("/etc/passwd")`, still gets rejected: it canonicalizes
    // to itself, which isn't under the project root. Also refuses: any extension other
    // than `.sql`; a budget of more than `MAX_SQL_FILE_READS_PER_FILE` reads per file
    // checked; files over `MAX_SQL_FILE_BYTES`; and anything that isn't a plain file (a
    // FIFO under the project root would otherwise hang the linter, since reading one
    // blocks until a writer opens it).
    fn read_sql_file(&self, path: &Path, reads_used: &mut u32) -> Option<String> {
        const MAX_SQL_FILE_BYTES: u64 = 256 * 1024;
        const MAX_SQL_FILE_READS_PER_FILE: u32 = 32;

        if !path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("sql"))
        {
            return None;
        }
        let root = self.project_root.as_ref()?;
        if *reads_used >= MAX_SQL_FILE_READS_PER_FILE {
            return None;
        }

        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            root.join(path)
        };
        let canonical_root = fs::canonicalize(root).ok()?;
        let canonical_target = fs::canonicalize(&candidate).ok()?;
        if !canonical_target.starts_with(&canonical_root) {
            return None;
        }

        let metadata = fs::metadata(&canonical_target).ok()?;
        if !metadata.is_file() || metadata.len() > MAX_SQL_FILE_BYTES {
            return None;
        }

        *reads_used += 1;
        fs::read_to_string(&canonical_target).ok()
    }

    // Build `string_var_candidates`: names bound to a plain string literal (or a
    // resolvable `.sql` file read) exactly once anywhere in the module. Runs once, as a
    // pre-pass over the whole module before `visit_stmt`'s single top-to-bottom walk —
    // NOT incrementally during that walk. Two reasons: (1) `visit_stmt` recurses into a
    // function body at its `def` site, so a module-level constant defined *after* the
    // function using it would otherwise be invisible; (2) `self.variables` already
    // tracks names flatly with no scope stack (see its own docs), so a bare pre-pass
    // over every binding site inherits that same simplicity for free — a name assigned
    // once in each of two different functions is two bindings of one flat name, and is
    // correctly excluded as ambiguous, rather than requiring real scope resolution.
    fn collect_string_var_candidates(&self, body: &[Stmt], path: &Path) -> HashMap<String, String> {
        let mut collector = StringBindingCollector {
            linter: self,
            current_file: path,
            reads_used: 0,
            bindings: HashMap::new(),
        };
        for stmt in body {
            collector.visit_stmt(stmt);
        }
        collector
            .bindings
            .into_iter()
            .filter_map(|(name, binding)| match binding {
                StringBinding::Literal(s) => Some((name, s)),
                StringBinding::Poisoned => None,
            })
            .collect()
    }

    // Extract dropped column names from a drop() call.
    fn extract_drop_columns(call: &ast::ExprCall) -> Option<Vec<String>> {
        // Check `columns=` kwarg first (pandas pattern — always correct for column drops)
        for keyword in &call.arguments.keywords {
            if keyword.arg.as_ref().map(|s| s.as_str()) == Some("columns") {
                return Self::extract_string_list_or_single(&keyword.value);
            }
        }

        // Check for axis kwarg
        let axis_kwarg = call
            .arguments
            .keywords
            .iter()
            .find(|k| k.arg.as_ref().map(|s| s.as_str()) == Some("axis"));

        if let Some(axis_kw) = axis_kwarg {
            // axis kwarg present — only drop columns when axis=1
            if let Expr::NumberLiteral(n) = &axis_kw.value {
                if let ast::Number::Int(ref i) = n.value {
                    if i.as_u64() == Some(1) {
                        if let Some(first_arg) = call.arguments.args.first() {
                            return Self::extract_string_list_or_single(first_arg);
                        }
                    }
                }
            }
            return None; // axis present but not 1 → row drop
        }

        // No axis kwarg → polars pattern, use first positional arg
        if let Some(first_arg) = call.arguments.args.first() {
            return Self::extract_string_list_or_single(first_arg);
        }

        None
    }

    // Extract rename mapping from a rename() call: {"old": "new", ...}.
    fn extract_rename_mapping(call: &ast::ExprCall) -> Option<HashMap<String, String>> {
        // Check `columns={"old": "new"}` kwarg (pandas)
        for keyword in &call.arguments.keywords {
            if keyword.arg.as_ref().map(|s| s.as_str()) == Some("columns") {
                if let Expr::Dict(dict) = &keyword.value {
                    return Self::extract_string_dict(dict);
                }
            }
        }
        // Fall back to first positional arg dict (polars)
        if let Some(Expr::Dict(dict)) = call.arguments.args.first() {
            return Self::extract_string_dict(dict);
        }
        None
    }

    // `str.lower` / `str.upper` referenced bare (not called) — the shape `rename(...)`
    // takes as its `columns=`/first-positional argument to case-fold every column,
    // rather than remap specific ones by name.
    fn expr_as_str_case_fold(expr: &Expr) -> Option<CaseFold> {
        if let Expr::Attribute(attr) = expr {
            if let Expr::Name(name) = &*attr.value {
                if name.id.as_str() == "str" {
                    return match attr.attr.as_str() {
                        "lower" => Some(CaseFold::Lower),
                        "upper" => Some(CaseFold::Upper),
                        _ => None,
                    };
                }
            }
        }
        None
    }

    // Extract a case-fold from a rename() call: `.rename(columns=str.lower)` (pandas)
    // or `.rename(str.lower)` (polars' first-positional convention, mirroring
    // extract_rename_mapping's dict handling above).
    fn extract_rename_case_fold(call: &ast::ExprCall) -> Option<CaseFold> {
        for keyword in &call.arguments.keywords {
            if keyword.arg.as_ref().map(|s| s.as_str()) == Some("columns") {
                return Self::expr_as_str_case_fold(&keyword.value);
            }
        }
        if let Some(first_arg) = call.arguments.args.first() {
            return Self::expr_as_str_case_fold(first_arg);
        }
        None
    }

    // `df.columns.str.lower()` / `.str.upper()` — the RHS shape of
    // `df.columns = df.columns.str.lower()`. Returns the receiver's variable name
    // (so the caller can confirm it matches the assignment target) and the fold.
    fn extract_columns_str_fold(value: &Expr) -> Option<(String, CaseFold)> {
        let Expr::Call(call) = value else {
            return None;
        };
        let Expr::Attribute(fold_attr) = &*call.func else {
            return None;
        };
        let fold = match fold_attr.attr.as_str() {
            "lower" => CaseFold::Lower,
            "upper" => CaseFold::Upper,
            _ => return None,
        };
        let Expr::Attribute(str_attr) = &*fold_attr.value else {
            return None;
        };
        if str_attr.attr.as_str() != "str" {
            return None;
        }
        let Expr::Attribute(columns_attr) = &*str_attr.value else {
            return None;
        };
        if columns_attr.attr.as_str() != "columns" {
            return None;
        }
        let Expr::Name(recv) = &*columns_attr.value else {
            return None;
        };
        Some((recv.id.to_string(), fold))
    }

    fn extract_string_dict(dict: &ast::ExprDict) -> Option<HashMap<String, String>> {
        let mut map = HashMap::new();
        for item in &dict.items {
            if let Some(key) = &item.key {
                match (
                    Self::extract_string_literal(key),
                    Self::extract_string_literal(&item.value),
                ) {
                    (Some(k), Some(v)) => {
                        map.insert(k.to_string(), v.to_string());
                    }
                    _ => return None, // Non-literal key or value
                }
            }
        }
        Some(map)
    }

    // Create a synthetic inferred schema and register it. Returns the schema name.
    fn make_inferred_schema(&mut self, cols: Vec<String>, var: &str, line: usize) -> String {
        let name = format!("__inferred_{}_at_{}", var, line);
        self.schemas.insert(name.clone(), cols);
        name
    }

    // Column membership check used by every column-access validator (see
    // `schema_has_column`'s call sites). Exact match: SQL-derived schemas already have
    // `self.sql_dialect`'s case-folding baked into their column names by
    // `sql::columns_from_select` at inference time, so e.g. a Snowflake query genuinely
    // produces `ORDER_ID`, and `df["order_id"]` is a real bug worth reporting, not a
    // false positive to suppress. Kept as a named helper (rather than inlining
    // `cols.iter().any(|c| c == col)` at each call site) so every validator agrees by
    // construction if this ever needs to change again.
    //
    // The one exception is an *open* schema (`self.open_schemas`, e.g. a Feast
    // retrieval result — see `register_feast_dataframe`): membership is unconditionally
    // `true` there, because the known column list is deliberately incomplete (Feast's
    // real output also includes entity_df's join keys and timestamp column, which
    // aren't resolvable in general) and treating it as exhaustive would manufacture
    // false unknown-column errors on real columns this checker just doesn't know about.
    fn schema_has_column(&self, schema_name: &str, col: &str) -> bool {
        if self.open_schemas.contains(schema_name) {
            return true;
        }
        self.schemas
            .get(schema_name)
            .is_some_and(|cols| cols.iter().any(|c| c == col))
    }

    // Extract a column name from a `pl.col("name")` or `col("name")` call expression.
    fn extract_pl_col_name(expr: &Expr) -> Option<String> {
        if let Expr::Call(call) = expr {
            let is_col_call = match &*call.func {
                Expr::Attribute(attr) => {
                    attr.attr.as_str() == "col"
                        && matches!(&*attr.value, Expr::Name(n) if matches!(n.id.as_str(), "pl" | "polars"))
                }
                Expr::Name(n) => n.id.as_str() == "col",
                _ => false,
            };
            if is_col_call {
                return call
                    .arguments
                    .args
                    .first()
                    .and_then(|a| Self::extract_string_literal(a))
                    .map(|s| s.to_string());
            }
        }
        None
    }

    // Recursively collect all column names referenced via `pl.col("name")` / `col("name")`
    // in an expression tree. Handles chained calls, lists, tuples, comparisons, and binary ops.
    fn collect_pl_col_names(expr: &Expr) -> Vec<String> {
        if let Some(name) = Self::extract_pl_col_name(expr) {
            return vec![name];
        }
        match expr {
            Expr::Call(call) => {
                let mut names = Vec::new();
                if let Expr::Attribute(attr) = &*call.func {
                    names.extend(Self::collect_pl_col_names(&attr.value));
                }
                for arg in &call.arguments.args {
                    names.extend(Self::collect_pl_col_names(arg));
                }
                for kw in &call.arguments.keywords {
                    names.extend(Self::collect_pl_col_names(&kw.value));
                }
                names
            }
            Expr::List(list) => list
                .elts
                .iter()
                .flat_map(Self::collect_pl_col_names)
                .collect(),
            Expr::Tuple(tuple) => tuple
                .elts
                .iter()
                .flat_map(Self::collect_pl_col_names)
                .collect(),
            Expr::Compare(compare) => {
                let mut names = Self::collect_pl_col_names(&compare.left);
                for comp in compare.comparators.iter() {
                    names.extend(Self::collect_pl_col_names(comp));
                }
                names
            }
            Expr::BinOp(binop) => {
                let mut names = Self::collect_pl_col_names(&binop.left);
                names.extend(Self::collect_pl_col_names(&binop.right));
                names
            }
            Expr::BoolOp(boolop) => boolop
                .values
                .iter()
                .flat_map(Self::collect_pl_col_names)
                .collect(),
            Expr::UnaryOp(unary) => Self::collect_pl_col_names(&unary.operand),
            _ => Vec::new(),
        }
    }

    // Does `expr` produce a value derived from a tainted variable — i.e. should its
    // assignment target also be considered tainted?  Two forms: a plain alias
    // (`x = df`) and a delegate call forwarding a tainted variable as the first
    // positional argument (`x = preproc(df)`, `x = infer(step1)`, …). This is what
    // lets taint follow a `step1 = preproc(df); step2 = infer(step1)` chain.
    //
    // Deliberately does NOT treat `x = df[["a", "b"]]` (a literal list/string slice)
    // as tainting `x`: that expression already produces a *fully known* schema for
    // `x` via the existing multi-column-subscript tracking in the Assign handler
    // above (make_inferred_schema), independent of this heuristic. A later
    // `x["bad_col"]` is therefore an outright, certain unknown-column bug local to
    // this function — caught directly by visit_expr against that inferred schema —
    // not an ambiguous "maybe missing from the caller" requirement to add to `df`'s
    // contract. Folding it in here would double-report the same bug two ways.
    fn expr_forwards_tainted(tainted: &std::collections::HashSet<String>, expr: &Expr) -> bool {
        match expr {
            Expr::Name(n) => tainted.contains(n.id.as_str()),
            Expr::Call(call) => call
                .arguments
                .args
                .first()
                .is_some_and(|a| matches!(a, Expr::Name(n) if tainted.contains(n.id.as_str()))),
            _ => false,
        }
    }

    // Recursively scan `expr` for two things, given the current set of variable names
    // considered aliases of the function's own first parameter:
    //   - `direct`: columns subscripted directly off a tainted variable, both a single
    //     string (`tainted["col"]`) and a list (`tainted[["a", "b"]]`)
    //   - `delegates`: names of functions called with a tainted variable as their first
    //     positional argument — candidates for transitive requirement resolution
    //     (see resolve_transitive_requires), since the parameter itself carries no
    //     schema here and we can't validate the access ourselves, only record it.
    fn scan_expr_for_contract(
        tainted: &std::collections::HashSet<String>,
        expr: &Expr,
        direct: &mut Vec<String>,
        delegates: &mut Vec<String>,
    ) {
        match expr {
            Expr::Subscript(subscript) => {
                if let Expr::Name(base) = &*subscript.value {
                    if tainted.contains(base.id.as_str()) {
                        if let Some(cols) = Self::extract_string_list_or_single(&subscript.slice) {
                            direct.extend(cols);
                        }
                    }
                }
                Self::scan_expr_for_contract(tainted, &subscript.value, direct, delegates);
                Self::scan_expr_for_contract(tainted, &subscript.slice, direct, delegates);
            }
            Expr::Attribute(attr) => {
                Self::scan_expr_for_contract(tainted, &attr.value, direct, delegates);
            }
            Expr::Call(call) => {
                let first_arg_tainted =
                    call.arguments.args.first().is_some_and(
                        |a| matches!(a, Expr::Name(n) if tainted.contains(n.id.as_str())),
                    );
                match &*call.func {
                    Expr::Name(func_name) if first_arg_tainted => {
                        delegates.push(func_name.id.to_string());
                    }
                    // `module.func(df)` via a plain `import module` — record the bare
                    // attribute name as a delegate candidate the same way a bare-name
                    // call would be (resolve_delegate_target follows it back to the
                    // module). Guarded on `base` not itself being tainted so an actual
                    // DataFrame method call (`df.merge(df)`) is never misread as a
                    // delegate to a function literally named `merge`.
                    Expr::Attribute(attr) if first_arg_tainted => {
                        if let Expr::Name(base) = &*attr.value {
                            if !tainted.contains(base.id.as_str()) {
                                delegates.push(attr.attr.to_string());
                            }
                        }
                    }
                    _ => {}
                }
                Self::scan_expr_for_contract(tainted, &call.func, direct, delegates);
                for arg in &call.arguments.args {
                    Self::scan_expr_for_contract(tainted, arg, direct, delegates);
                }
                for kw in &call.arguments.keywords {
                    Self::scan_expr_for_contract(tainted, &kw.value, direct, delegates);
                }
            }
            Expr::BinOp(binop) => {
                Self::scan_expr_for_contract(tainted, &binop.left, direct, delegates);
                Self::scan_expr_for_contract(tainted, &binop.right, direct, delegates);
            }
            Expr::BoolOp(boolop) => {
                for v in &boolop.values {
                    Self::scan_expr_for_contract(tainted, v, direct, delegates);
                }
            }
            Expr::UnaryOp(unary) => {
                Self::scan_expr_for_contract(tainted, &unary.operand, direct, delegates);
            }
            Expr::Compare(compare) => {
                Self::scan_expr_for_contract(tainted, &compare.left, direct, delegates);
                for comp in compare.comparators.iter() {
                    Self::scan_expr_for_contract(tainted, comp, direct, delegates);
                }
            }
            Expr::List(list) => {
                for el in &list.elts {
                    Self::scan_expr_for_contract(tainted, el, direct, delegates);
                }
            }
            Expr::Tuple(tuple) => {
                for el in &tuple.elts {
                    Self::scan_expr_for_contract(tainted, el, direct, delegates);
                }
            }
            _ => {}
        }
    }

    // Statement-level counterpart to scan_expr_for_contract — covers the handful of
    // statement shapes that appear in typical single-purpose transform functions
    // (return, bare expr, assign, if/for/while/with). Not an exhaustive AST walk;
    // deeply nested control flow may under-report requirements/delegates. `tainted`
    // grows as assignments are discovered, so later statements in the same body see
    // earlier aliasing — this is a single top-to-bottom pass, not full control-flow
    // analysis: taint picked up inside an `if`/`for` body is visible afterwards, and
    // both branches of an `if` contribute to the same taint set (conservative).
    fn analyze_stmt_for_contract(
        stmt: &Stmt,
        tainted: &mut std::collections::HashSet<String>,
        direct: &mut Vec<String>,
        delegates: &mut Vec<String>,
    ) {
        match stmt {
            Stmt::Return(ret) => {
                if let Some(value) = &ret.value {
                    Self::scan_expr_for_contract(tainted, value, direct, delegates);
                }
            }
            Stmt::Expr(expr_stmt) => {
                Self::scan_expr_for_contract(tainted, &expr_stmt.value, direct, delegates);
            }
            Stmt::Assign(assign) => {
                Self::scan_expr_for_contract(tainted, &assign.value, direct, delegates);
                if Self::expr_forwards_tainted(tainted, &assign.value) {
                    for target in &assign.targets {
                        if let Expr::Name(t) = target {
                            tainted.insert(t.id.to_string());
                        }
                    }
                }
            }
            Stmt::AnnAssign(ann_assign) => {
                if let Some(value) = &ann_assign.value {
                    Self::scan_expr_for_contract(tainted, value, direct, delegates);
                    if Self::expr_forwards_tainted(tainted, value) {
                        if let Expr::Name(t) = &*ann_assign.target {
                            tainted.insert(t.id.to_string());
                        }
                    }
                }
            }
            Stmt::If(if_stmt) => {
                Self::scan_expr_for_contract(tainted, &if_stmt.test, direct, delegates);
                for s in &if_stmt.body {
                    Self::analyze_stmt_for_contract(s, tainted, direct, delegates);
                }
                for clause in &if_stmt.elif_else_clauses {
                    for s in &clause.body {
                        Self::analyze_stmt_for_contract(s, tainted, direct, delegates);
                    }
                }
            }
            Stmt::For(for_stmt) => {
                Self::scan_expr_for_contract(tainted, &for_stmt.iter, direct, delegates);
                for s in &for_stmt.body {
                    Self::analyze_stmt_for_contract(s, tainted, direct, delegates);
                }
            }
            Stmt::While(while_stmt) => {
                Self::scan_expr_for_contract(tainted, &while_stmt.test, direct, delegates);
                for s in &while_stmt.body {
                    Self::analyze_stmt_for_contract(s, tainted, direct, delegates);
                }
            }
            Stmt::With(with_stmt) => {
                for item in &with_stmt.items {
                    Self::scan_expr_for_contract(tainted, &item.context_expr, direct, delegates);
                }
                for s in &with_stmt.body {
                    Self::analyze_stmt_for_contract(s, tainted, direct, delegates);
                }
            }
            _ => {}
        }
    }

    // Validate a call to a function with a known parameter contract (`self.param_requires`)
    // against the tracked schema of its first positional argument.  Fires at the call
    // site — pipeline.py, not inside the function itself — because that's where the
    // argument's actual inferred schema is known.
    fn check_call_requirements(
        &self,
        func_name: &str,
        call: &ast::ExprCall,
        line: usize,
        col: usize,
        errors: &mut Vec<LintError>,
    ) {
        let Some((required, origin)) = self.param_requires.get(func_name) else {
            return;
        };
        let Some(first_arg) = call.arguments.args.first() else {
            return;
        };
        let Expr::Name(arg_name) = first_arg else {
            return;
        };
        let Some((schema_name, _)) = self.variables.get(arg_name.id.as_str()) else {
            return;
        };
        let Some(available) = self.schemas.get(schema_name) else {
            return;
        };
        let missing: Vec<&String> = required.iter().filter(|c| !available.contains(c)).collect();
        if missing.is_empty() {
            return;
        }
        let missing_str = missing
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let available_str = available.join(", ");
        let required_str = required.join(", ");
        errors.push(LintError {
            line,
            col,
            code: CODE_MISSING_COLUMN.to_string(),
            message: format!(
                "'{}' passed to {} is missing column(s) {{{}}} — available: {{{}}}, required: {{{}}}",
                arg_name.id.as_str(),
                origin,
                missing_str,
                available_str,
                required_str
            ),
            severity: "error".to_string(),
        });
    }

    // Validate any `pl.col("name")` / `col("name")` references in a call's arguments
    // against the schema of a tracked receiver variable.
    fn validate_pl_col_args_on_receiver(
        &self,
        recv_name: &str,
        call: &ast::ExprCall,
        line: usize,
        col: usize,
        errors: &mut Vec<LintError>,
    ) {
        let Some((schema_name, defined_line)) =
            self.variables.get(recv_name).map(|(s, l)| (s.clone(), *l))
        else {
            return;
        };
        let Some(columns) = self.schemas.get(&schema_name).cloned() else {
            return;
        };
        let col_names: Vec<String> = call
            .arguments
            .args
            .iter()
            .flat_map(Self::collect_pl_col_names)
            .chain(
                call.arguments
                    .keywords
                    .iter()
                    .flat_map(|kw| Self::collect_pl_col_names(&kw.value)),
            )
            .collect();
        for col_name in col_names {
            if !columns.contains(&col_name) {
                let schema_display = self.schema_display(&schema_name, defined_line);
                let mut message =
                    format!("Column '{}' does not exist in {}", col_name, schema_display);
                if let Some(suggestion) = find_best_match(&col_name, &columns) {
                    message.push_str(&format!(" (did you mean '{}'?)", suggestion));
                }
                errors.push(LintError {
                    line,
                    col,
                    code: CODE_UNKNOWN_COLUMN.to_string(),
                    message,
                    severity: "error".to_string(),
                });
            }
        }
    }

    // Remove a column in-place from `recv`'s schema. Used for `del df['col']` and `df.pop('col')`.
    fn remove_column_inplace(
        &mut self,
        recv: &str,
        col_name: &str,
        line: usize,
        col: usize,
        context: &str,
        errors: &mut Vec<LintError>,
    ) {
        let base_info = self.variables.get(recv).map(|(s, l)| (s.clone(), *l));
        let Some((schema_name, def_line)) = base_info else {
            return;
        };
        let schema_display = self.schema_display(&schema_name, def_line);
        let Some(cols) = self.schemas.get(&schema_name).cloned() else {
            return;
        };
        if !cols.contains(&col_name.to_string()) {
            errors.push(LintError {
                line,
                col,
                code: CODE_UNKNOWN_COLUMN.to_string(),
                message: format!(
                    "Column '{}' does not exist in {} ({})",
                    col_name, schema_display, context
                ),
                severity: "error".to_string(),
            });
        } else {
            let new_cols: Vec<String> = cols
                .into_iter()
                .filter(|c| c.as_str() != col_name)
                .collect();
            let new_schema = self.make_inferred_schema(new_cols, recv, line);
            self.variables.insert(recv.to_string(), (new_schema, line));
        }
    }

    // Add a column in-place to `recv`'s schema. Used for `df.insert(loc, col, value)`.
    fn add_column_inplace(&mut self, recv: &str, col_name: &str, line: usize) {
        let base_info = self.variables.get(recv).map(|(s, l)| (s.clone(), *l));
        let Some((schema_name, _)) = base_info else {
            return;
        };
        let mut cols = self.schemas.get(&schema_name).cloned().unwrap_or_default();
        if !cols.contains(&col_name.to_string()) {
            cols.push(col_name.to_string());
            let new_schema = self.make_inferred_schema(cols, recv, line);
            self.variables.insert(recv.to_string(), (new_schema, line));
        }
    }

    // Walk a statement node, updating linter state and collecting diagnostics.
    //
    // ClassDef      — detect BaseSchema subclasses; collect inherited + declared columns.
    // FunctionDef   — record annotated return types for cross-assignment schema tracking.
    // Assign        — track load calls, method-chain results (drop/rename/select/…),
    //                 DataFrame[Schema](...) instantiation, and merge/concat.
    // AnnAssign     — handle `df: Annotated[pd.DataFrame, S]` and quoted annotations.
    // Expr          — delegate column-access checks to visit_expr.
    // Delete        — handle `del df["col"]` in-place mutations.
    fn visit_stmt(&mut self, stmt: &Stmt, errors: &mut Vec<LintError>) {
        match stmt {
            Stmt::ClassDef(class_def) => {
                let is_schema = class_def.bases().iter().any(|base| match base {
                    Expr::Attribute(attr) => Self::is_schema_base(attr.attr.as_str()),
                    Expr::Name(name) => {
                        Self::is_schema_base(name.id.as_str())
                            || self.schemas.contains_key(name.id.as_str())
                    }
                    _ => false,
                });

                if is_schema {
                    // Collect inherited columns first (multiple-inheritance support).
                    // Each named base that is already registered as a schema contributes
                    // its columns; later bases can shadow earlier ones by appending, but
                    // duplicate column names are left for the schema author to resolve.
                    let mut columns = Vec::new();
                    for base in class_def.bases() {
                        if let Expr::Name(name) = base {
                            if let Some(parent_cols) = self.schemas.get(name.id.as_str()) {
                                columns.extend(parent_cols.clone());
                            }
                        }
                    }
                    // Walk the class body to extract column definitions.
                    // Three declaration forms are supported:
                    //   1. `col: Column(...)` / `col = Column(...)` — explicit column,
                    //      may have an `alias=` keyword that overrides the attribute name.
                    //   2. `col: ColumnSet(members=[...])` — a named group that also
                    //      expands its member strings as individual columns.
                    //   3. Any other annotated attribute — treated as a plain column
                    //      whose name equals the attribute name.
                    for body_stmt in &class_def.body {
                        if let Stmt::AnnAssign(ann_assign) = body_stmt {
                            if let Expr::Name(name) = ann_assign.target.as_ref() {
                                let mut col_added = false;
                                if let Some(value) = &ann_assign.value {
                                    if let Expr::Call(call) = &**value {
                                        let func_name = match &*call.func {
                                            Expr::Name(n) => Some(n.id.as_str()),
                                            Expr::Attribute(a) => Some(a.attr.as_str()),
                                            _ => None,
                                        };

                                        if let Some(f) = func_name {
                                            if f == "Column" {
                                                let mut alias = None;
                                                for keyword in call.arguments.keywords.iter() {
                                                    if keyword.arg.as_ref().map(|s| s.as_str())
                                                        == Some("alias")
                                                    {
                                                        if let Some(s) =
                                                            Self::extract_string_literal(
                                                                &keyword.value,
                                                            )
                                                        {
                                                            alias = Some(s.to_string());
                                                        }
                                                    }
                                                }
                                                let col_name =
                                                    alias.unwrap_or_else(|| name.id.to_string());
                                                columns.push(col_name);
                                                col_added = true;
                                            } else if f == "ColumnSet" || f == "ColumnGroup" {
                                                columns.push(name.id.to_string());
                                                for keyword in call.arguments.keywords.iter() {
                                                    if keyword.arg.as_ref().map(|s| s.as_str())
                                                        == Some("members")
                                                    {
                                                        if let Expr::List(list) = &keyword.value {
                                                            for el in &list.elts {
                                                                if let Some(s) =
                                                                    Self::extract_string_literal(el)
                                                                {
                                                                    columns.push(s.to_string());
                                                                } else if let Expr::Name(n) = el {
                                                                    columns.push(n.id.to_string());
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                                col_added = true;
                                            }
                                        }
                                    }
                                }
                                if !col_added {
                                    columns.push(name.id.to_string());
                                }
                            }
                        } else if let Stmt::Assign(assign) = body_stmt {
                            for target in &assign.targets {
                                if let Expr::Name(name) = target {
                                    let mut col_added = false;
                                    if let Expr::Call(call) = &*assign.value {
                                        let func_name = match &*call.func {
                                            Expr::Name(n) => Some(n.id.as_str()),
                                            Expr::Attribute(a) => Some(a.attr.as_str()),
                                            _ => None,
                                        };

                                        if let Some(f) = func_name {
                                            if f == "Column" {
                                                let mut alias = None;
                                                for keyword in call.arguments.keywords.iter() {
                                                    if keyword.arg.as_ref().map(|s| s.as_str())
                                                        == Some("alias")
                                                    {
                                                        if let Some(s) =
                                                            Self::extract_string_literal(
                                                                &keyword.value,
                                                            )
                                                        {
                                                            alias = Some(s.to_string());
                                                        }
                                                    }
                                                }
                                                columns.push(
                                                    alias.unwrap_or_else(|| name.id.to_string()),
                                                );
                                                col_added = true;
                                            } else if f == "ColumnSet" || f == "ColumnGroup" {
                                                columns.push(name.id.to_string());
                                                for keyword in call.arguments.keywords.iter() {
                                                    if keyword.arg.as_ref().map(|s| s.as_str())
                                                        == Some("members")
                                                    {
                                                        if let Expr::List(list) = &keyword.value {
                                                            for el in &list.elts {
                                                                if let Some(s) =
                                                                    Self::extract_string_literal(el)
                                                                {
                                                                    columns.push(s.to_string());
                                                                } else if let Expr::Name(n) = el {
                                                                    columns.push(n.id.to_string());
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                                col_added = true;
                                            }
                                        }
                                    }
                                    if !col_added {
                                        columns.push(name.id.to_string());
                                    }
                                }
                            }
                        }
                    }
                    // Deduplicate columns (MI may bring overlapping columns)
                    columns.sort();
                    columns.dedup();
                    // Warn about column names that conflict with reserved methods
                    for col_name in &columns {
                        if RESERVED_METHODS.contains(&col_name.as_str()) {
                            let (line, col) = self.source_location(class_def.range().start());
                            errors.push(LintError {
                                line,
                                col,
                                code: CODE_RESERVED_NAME.to_string(),
                                message: format!(
                                    "Column name '{}' in {} conflicts with a pandas/polars method. This will shadow the method when accessed via attribute syntax (df.{}). Consider renaming to '{}_value' or similar.",
                                    col_name, class_def.name, col_name, col_name
                                ),
                                severity: "error".to_string(),
                            });
                        }
                    }
                    self.schemas.insert(class_def.name.to_string(), columns);
                } else if Self::class_body_has_tablename(class_def) {
                    // SQLAlchemy declarative model: `class Order(Base): __tablename__ =
                    // "orders"; ...`. Detected structurally, via `__tablename__` in the
                    // class's own body, rather than by base-class name — the declarative
                    // base is normally imported from a project-local module
                    // (`class Order(Base)`), so it's never one of the fixed names
                    // `is_schema_base` recognizes. Uses a separate, allowlist-based
                    // extractor rather than the permissive one above: see
                    // `extract_orm_columns` for why (its "any annotated attribute is a
                    // column" fallback would sweep in `relationship(...)` attributes,
                    // `__table_args__`, etc., which aren't real database columns).
                    let columns = Self::extract_orm_columns(class_def);
                    // Deliberately no RESERVED_METHODS conflict check here: that
                    // warning is authoring advice for typedframes-native schemas
                    // ("rename the column"), but a mapped class's column names come
                    // from an external database schema the user doesn't control.
                    self.schemas.insert(class_def.name.to_string(), columns);
                }
            }
            Stmt::FunctionDef(func_def) => {
                let (fn_def_line, _) = self.source_location(func_def.range().start());

                // Track return type annotations like -> PandasFrame[Schema]
                if let Some(returns) = &func_def.returns {
                    if let Some(schema_name) = Self::extract_schema_from_annotation(returns) {
                        self.functions
                            .insert(func_def.name.to_string(), schema_name.to_string());
                    }
                }

                // Schema-annotated parameters (`def f(df: PandasFrame[Schema])`,
                // `Annotated[pd.DataFrame, Schema]`, or a quoted equivalent) get tracked
                // in self.variables exactly like a local `df: PandasFrame[Schema] = ...`
                // assignment would — so accesses inside the body are validated against
                // the declared schema, the same as anywhere else in the file, rather
                // than left unchecked just because the binding came from a parameter.
                for p in func_def
                    .parameters
                    .posonlyargs
                    .iter()
                    .chain(func_def.parameters.args.iter())
                    .chain(func_def.parameters.kwonlyargs.iter())
                {
                    if let Some(annotation) = &p.parameter.annotation {
                        if let Some(schema_name) = Self::extract_schema_from_annotation(annotation)
                        {
                            self.variables.insert(
                                p.parameter.name.id.to_string(),
                                (schema_name.to_string(), fn_def_line),
                            );
                        }
                    }
                }

                for body_stmt in &func_def.body {
                    self.visit_stmt(body_stmt, errors);
                }
                // If no annotation-based mapping, infer from `return <var>`.
                // After visiting the body, self.variables holds the schema of every
                // local variable; look up the returned one and register the function.
                if !self.functions.contains_key(func_def.name.as_str()) {
                    if let Some(var_name) = find_returned_var(&func_def.body) {
                        if let Some((schema_name, _)) = self.variables.get(&var_name) {
                            let schema_name = schema_name.clone();
                            if self.schemas.contains_key(&schema_name) {
                                self.functions
                                    .insert(func_def.name.to_string(), schema_name);
                            }
                        }
                    }
                }
                // Infer a column *contract* for the function's first parameter: every
                // column subscripted directly off that parameter or a variable derived
                // from it (`param["col"]`, or `x["col"]` where `x = param` / `x = f(param)`),
                // plus every function this one delegates its own parameter to. The
                // parameter itself carries no schema here — the caller's variable does —
                // so this is only a requirement to check against later, not something we
                // can validate inside the function body itself. Delegate targets are
                // resolved to a *transitive* union only at the project-index level (see
                // resolve_transitive_requires) — this pass records only the direct
                // requirements and the raw delegate names.
                //
                // If the first parameter carries a schema annotation, its declared column
                // list replaces the heuristic body-scan as the *direct* requirement — the
                // annotation is an explicit, complete contract, whereas the body-scan is
                // only a best-effort proxy used when no such contract exists. Delegates are
                // still collected and unioned in as usual: columns a forwarded call needs
                // only make the contract stricter, never wrong.
                if let Some(first_param) = func_def
                    .parameters
                    .posonlyargs
                    .first()
                    .or_else(|| func_def.parameters.args.first())
                {
                    let param_name = first_param.parameter.name.id.as_str();
                    let mut tainted = std::collections::HashSet::new();
                    tainted.insert(param_name.to_string());
                    let mut required = Vec::new();
                    let mut delegates = Vec::new();
                    for body_stmt in &func_def.body {
                        Self::analyze_stmt_for_contract(
                            body_stmt,
                            &mut tainted,
                            &mut required,
                            &mut delegates,
                        );
                    }

                    // Capture the annotation's schema *name* unconditionally, even when
                    // it can't be resolved to columns yet (the schema may live in a
                    // third file, not yet visible to this single-file Linter pass).
                    // resolve_transitive_requires resolves it project-wide once every
                    // file's schemas are known — see param_schema_name on IndexFunction.
                    let annotation_schema_name: Option<String> = first_param
                        .parameter
                        .annotation
                        .as_ref()
                        .and_then(|a| Self::extract_schema_from_annotation(a))
                        .map(|s| s.to_string());
                    if let Some(name) = &annotation_schema_name {
                        self.param_schema_names
                            .insert(func_def.name.to_string(), (name.clone(), fn_def_line));
                    }
                    let annotated_cols = annotation_schema_name
                        .as_deref()
                        .and_then(|schema_name| self.schemas.get(schema_name).cloned());
                    if let Some(cols) = annotated_cols {
                        required = cols;
                    }

                    required.sort();
                    required.dedup();
                    delegates.sort();
                    delegates.dedup();
                    if !required.is_empty() || !delegates.is_empty() {
                        self.requires
                            .insert(func_def.name.to_string(), (required.clone(), fn_def_line));
                        if !delegates.is_empty() {
                            self.delegates.insert(func_def.name.to_string(), delegates);
                        }
                        // Local (same-file) callers only ever see the *direct* set here —
                        // the transitive union (via delegates) is resolved once, project-wide,
                        // in build_index_internal, and reaches other files' callers through
                        // the cross-file entry (see load_cross_file_symbols).
                        if !required.is_empty() {
                            let origin = format!(
                                "{} ({}:{})",
                                func_def.name.as_str(),
                                self.file_display,
                                fn_def_line
                            );
                            self.param_requires
                                .insert(func_def.name.to_string(), (required, origin));
                        }
                    }
                }
            }
            Stmt::Assign(assign) => {
                let (current_line, current_col) = self.source_location(assign.range().start());

                // Check for mutations: df["new_col"] = ...
                for target in &assign.targets {
                    if let Expr::Subscript(subscript) = target {
                        if let Expr::Name(name) = &*subscript.value {
                            if let Some((schema_name, _)) = self.variables.get(name.id.as_str()) {
                                if let Some(col_name) =
                                    Self::extract_string_literal(&subscript.slice)
                                {
                                    let schema_name = schema_name.clone();
                                    let already_has_col =
                                        self.schema_has_column(&schema_name, col_name);
                                    if let Some(columns) = self.schemas.get_mut(&schema_name) {
                                        if !already_has_col {
                                            errors.push(LintError {
                                                line: current_line,
                                                col: current_col,
                                                code: CODE_UNKNOWN_COLUMN.to_string(),
                                                message: format!("Column '{}' does not exist in {} (mutation tracking)", col_name, schema_name),
                                                severity: "error".to_string(),
                                            });
                                            columns.push(col_name.to_string());
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // df.columns = df.columns.str.lower() / .str.upper() -- a recognized
                // case-fold of an already-known column set (see CaseFold's doc comment
                // for why this is a fixed, narrow pattern rather than general support
                // for arbitrary transform functions).
                for target in &assign.targets {
                    if let Expr::Attribute(target_attr) = target {
                        if target_attr.attr.as_str() != "columns" {
                            continue;
                        }
                        let Expr::Name(target_recv) = &*target_attr.value else {
                            continue;
                        };
                        let Some((rhs_recv, fold)) = Self::extract_columns_str_fold(&assign.value)
                        else {
                            continue;
                        };
                        if rhs_recv != target_recv.id.as_str() {
                            continue;
                        }
                        if let Some((schema_name, _)) =
                            self.variables.get(target_recv.id.as_str()).cloned()
                        {
                            if let Some(base_cols) = self.schemas.get(&schema_name).cloned() {
                                let new_cols: Vec<String> =
                                    base_cols.iter().map(|c| fold.apply(c)).collect();
                                let new_schema = self.make_inferred_schema(
                                    new_cols,
                                    target_recv.id.as_str(),
                                    current_line,
                                );
                                self.variables
                                    .insert(target_recv.id.to_string(), (new_schema, current_line));
                            }
                        }
                    }
                }

                // A. Multi-column subscript: a = b[["foo", "bar"]]
                if let Expr::Subscript(sub) = &*assign.value {
                    if let Expr::Name(base_name) = &*sub.value {
                        let base_str = base_name.id.as_str();
                        match Self::extract_string_list(&sub.slice) {
                            Some(cols) => {
                                let base_info =
                                    self.variables.get(base_str).map(|(s, l)| (s.clone(), *l));
                                if let Some((base_schema, base_def_line)) = &base_info {
                                    let base_cols =
                                        self.schemas.get(base_schema).cloned().unwrap_or_default();
                                    if !base_cols.is_empty() {
                                        for col in &cols {
                                            if !base_cols.contains(col) {
                                                let schema_display = self
                                                    .schema_display(base_schema, *base_def_line);
                                                errors.push(LintError {
                                                    line: current_line,
                                                    col: current_col,
                                                    code: CODE_UNKNOWN_COLUMN.to_string(),
                                                    message: format!(
                                                        "Column '{}' does not exist in {}",
                                                        col, schema_display
                                                    ),
                                                    severity: "error".to_string(),
                                                });
                                            }
                                        }
                                    }
                                }
                                let target_names: Vec<String> = assign
                                    .targets
                                    .iter()
                                    .filter_map(|t| {
                                        if let Expr::Name(n) = t {
                                            Some(n.id.to_string())
                                        } else {
                                            None
                                        }
                                    })
                                    .collect();
                                let var_name = target_names
                                    .first()
                                    .map(|s| s.as_str())
                                    .unwrap_or("unknown");
                                let schema_name =
                                    self.make_inferred_schema(cols, var_name, current_line);
                                for name in &target_names {
                                    self.variables
                                        .insert(name.clone(), (schema_name.clone(), current_line));
                                }
                            }
                            None => {
                                // Boolean mask / unknown — passthrough base schema to target
                                if let Some((base_schema, _)) =
                                    self.variables.get(base_str).map(|(s, l)| (s.clone(), *l))
                                {
                                    let target_names: Vec<String> = assign
                                        .targets
                                        .iter()
                                        .filter_map(|t| {
                                            if let Expr::Name(n) = t {
                                                Some(n.id.to_string())
                                            } else {
                                                None
                                            }
                                        })
                                        .collect();
                                    for name in &target_names {
                                        self.variables.insert(
                                            name.clone(),
                                            (base_schema.clone(), current_line),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }

                if let Expr::Call(call) = &*assign.value {
                    // Handle stmt = select(Order.id, Order.amount) — and the same
                    // chained onto .where(...)/.order_by(...)/etc, see
                    // SELECT_CHAIN_METHODS — so a later pd.read_sql(stmt, engine) can
                    // resolve `stmt` via `stmt_var_candidates`. Checked unconditionally
                    // here (rather than inside the match below) since the outermost
                    // call in the chained form is a `.where(...)` *method* call, not a
                    // bare `select(...)` name call, and would otherwise never reach the
                    // `Expr::Name(func_name)` arm at all. A later reassignment of the
                    // same name simply overwrites this entry (consistent with how
                    // `self.variables` already behaves) rather than needing the
                    // whole-module poisoning discipline `string_var_candidates` relies
                    // on — this only has to be correct in top-to-bottom order, like
                    // every other variable binding this checker tracks.
                    if let [Expr::Name(target_name)] = assign.targets.as_slice() {
                        if let Some(cols) = self.extract_select_columns(&assign.value) {
                            self.stmt_var_candidates
                                .insert(target_name.id.to_string(), cols);
                        }
                    }

                    let mut is_merge_or_concat = false;
                    let mut merge_schema = None;

                    match &*call.func {
                        Expr::Attribute(attr) => {
                            let func_name = attr.attr.as_str();
                            if func_name == "merge" {
                                if let Expr::Name(left_name) = &*attr.value {
                                    if let Some((left_schema, _)) =
                                        self.variables.get(left_name.id.as_str())
                                    {
                                        if !call.arguments.args.is_empty() {
                                            if let Expr::Name(right_name) = &call.arguments.args[0]
                                            {
                                                if let Some((right_schema, _)) =
                                                    self.variables.get(right_name.id.as_str())
                                                {
                                                    is_merge_or_concat = true;
                                                    merge_schema = Some((
                                                        left_schema.clone(),
                                                        right_schema.clone(),
                                                    ));
                                                }
                                            }
                                        }
                                    }
                                }
                            } else if func_name == "concat" {
                                if !call.arguments.args.is_empty() {
                                    if let Expr::List(list) = &call.arguments.args[0] {
                                        let mut schemas = Vec::new();
                                        for el in &list.elts {
                                            if let Expr::Name(n) = el {
                                                if let Some((s, _)) =
                                                    self.variables.get(n.id.as_str())
                                                {
                                                    schemas.push(s.clone());
                                                }
                                            }
                                        }
                                        if schemas.len() >= 2 {
                                            is_merge_or_concat = true;
                                            merge_schema =
                                                Some((schemas[0].clone(), schemas[1].clone()));
                                        }
                                    }
                                }
                            } else if func_name == "from_schema"
                                || func_name == "from_pandas"
                                || func_name == "from_polars"
                                || LOAD_FUNCTIONS.contains(&func_name)
                            {
                                // PandasFrame.from_schema(df, Schema) or Schema.from_pandas(df)
                                if let Expr::Attribute(inner_attr) = &*attr.value {
                                    // This is like PandasFrame.from_schema
                                    let class_name = inner_attr.attr.as_str();
                                    if class_name == "PandasFrame" || class_name == "PolarsFrame" {
                                        // Find the schema argument
                                        if call.arguments.args.len() >= 2 {
                                            if let Expr::Name(schema_name) = &call.arguments.args[1]
                                            {
                                                for target in &assign.targets {
                                                    if let Expr::Name(target_name) = target {
                                                        self.variables.insert(
                                                            target_name.id.to_string(),
                                                            (
                                                                schema_name.id.to_string(),
                                                                current_line,
                                                            ),
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                    }
                                } else if let Expr::Name(class_name) = &*attr.value {
                                    let class_str = class_name.id.as_str();
                                    if self.schemas.contains_key(class_str) {
                                        // Schema.from_pandas(df) style
                                        self.dataframes_total += 1;
                                        self.dataframes_typed += 1;
                                        for target in &assign.targets {
                                            if let Expr::Name(target_name) = target {
                                                self.variables.insert(
                                                    target_name.id.to_string(),
                                                    (class_str.to_string(), current_line),
                                                );
                                            }
                                        }
                                    } else if LOAD_MODULES.contains(&class_str)
                                        && LOAD_FUNCTIONS.contains(&func_name)
                                    {
                                        // pd.read_csv() / pl.scan_parquet() / pd.read_sql() etc.
                                        self.dataframes_total += 1;
                                        let (extracted, load_kind) =
                                            self.extract_load_columns(func_name, call);
                                        match extracted {
                                            Some(cols) => {
                                                self.dataframes_typed += 1;
                                                let target_names: Vec<String> = assign
                                                    .targets
                                                    .iter()
                                                    .filter_map(|t| {
                                                        if let Expr::Name(n) = t {
                                                            Some(n.id.to_string())
                                                        } else {
                                                            None
                                                        }
                                                    })
                                                    .collect();
                                                let var_name = target_names
                                                    .first()
                                                    .map(|s| s.as_str())
                                                    .unwrap_or("df");
                                                let schema_name = self.make_inferred_schema(
                                                    cols,
                                                    var_name,
                                                    current_line,
                                                );
                                                for name in &target_names {
                                                    self.variables.insert(
                                                        name.clone(),
                                                        (schema_name.clone(), current_line),
                                                    );
                                                }
                                            }
                                            None => {
                                                let hint = match load_kind {
                                                    LoadKind::Sql => {
                                                        "name the columns in the `SELECT` list \
                                                         (avoid `SELECT *`) or annotate: \
                                                         `df: Annotated[pd.DataFrame, MySchema] \
                                                         = pd.read_sql(...)`"
                                                    }
                                                    LoadKind::File => {
                                                        "specify `usecols`/`columns` or \
                                                         annotate: `df: Annotated[pd.DataFrame, \
                                                         MySchema] = pd.read_csv(...)`"
                                                    }
                                                    LoadKind::Orm => {
                                                        "pass select(Model.col1, Model.col2, ...) \
                                                         referencing a registered model's known \
                                                         columns (a bare `select(Model)` isn't \
                                                         supported), or annotate: \
                                                         `df: Annotated[pd.DataFrame, MySchema] \
                                                         = pd.read_sql(...)`"
                                                    }
                                                };
                                                errors.push(LintError {
                                                    line: current_line,
                                                    col: current_col,
                                                    code: CODE_UNTRACKED_DATAFRAME.to_string(),
                                                    message: format!(
                                                        "columns unknown at lint time; {hint}"
                                                    ),
                                                    severity: "warning".to_string(),
                                                });
                                            }
                                        }
                                    } else if CONNECTORX_MODULES.contains(&class_str)
                                        && func_name == "read_sql"
                                    {
                                        // connectorx.read_sql(conn_uri, sql) — SQL is
                                        // the second positional argument, the reverse of
                                        // pandas' convention, so this can't reuse
                                        // extract_load_columns/extract_sql_literal.
                                        self.dataframes_total += 1;
                                        let sql = call
                                            .arguments
                                            .args
                                            .get(1)
                                            .and_then(|a| self.extract_sql_expr(a));
                                        let cols =
                                            sql.and_then(|sql| {
                                                match sql::columns_from_select(
                                                    &sql,
                                                    self.sql_dialect,
                                                ) {
                                                    sql::SqlOutcome::Columns(cols) => Some(cols),
                                                    sql::SqlOutcome::Wildcard
                                                    | sql::SqlOutcome::Unparsed => None,
                                                }
                                            });
                                        match cols {
                                            Some(cols) => {
                                                self.dataframes_typed += 1;
                                                let target_names: Vec<String> = assign
                                                    .targets
                                                    .iter()
                                                    .filter_map(|t| {
                                                        if let Expr::Name(n) = t {
                                                            Some(n.id.to_string())
                                                        } else {
                                                            None
                                                        }
                                                    })
                                                    .collect();
                                                let var_name = target_names
                                                    .first()
                                                    .map(|s| s.as_str())
                                                    .unwrap_or("df");
                                                let schema_name = self.make_inferred_schema(
                                                    cols,
                                                    var_name,
                                                    current_line,
                                                );
                                                for name in &target_names {
                                                    self.variables.insert(
                                                        name.clone(),
                                                        (schema_name.clone(), current_line),
                                                    );
                                                }
                                            }
                                            None => {
                                                errors.push(LintError {
                                                    line: current_line,
                                                    col: current_col,
                                                    code: CODE_UNTRACKED_DATAFRAME.to_string(),
                                                    message: "columns unknown at lint \
                                                              time; name the columns in \
                                                              the `SELECT` list (avoid \
                                                              `SELECT *`) or annotate: \
                                                              `df: Annotated[pd.DataFrame, \
                                                              MySchema] = ...`"
                                                        .to_string(),
                                                    severity: "warning".to_string(),
                                                });
                                            }
                                        }
                                    }
                                }
                            } else if ROW_PASSTHROUGH_METHODS.contains(&func_name) {
                                // Row-preserving ops: propagate base schema unchanged
                                if let Expr::Name(recv) = &*attr.value {
                                    if let Some((base_schema, _)) =
                                        self.variables.get(recv.id.as_str())
                                    {
                                        let base_schema = base_schema.clone();
                                        for target in &assign.targets {
                                            if let Expr::Name(target_name) = target {
                                                self.variables.insert(
                                                    target_name.id.to_string(),
                                                    (base_schema.clone(), current_line),
                                                );
                                            }
                                        }
                                    }
                                }
                            } else if func_name == "select" {
                                if let Expr::Name(recv) = &*attr.value {
                                    let recv_str = recv.id.as_str();
                                    let base_info =
                                        self.variables.get(recv_str).map(|(s, l)| (s.clone(), *l));
                                    let base_cols = base_info
                                        .as_ref()
                                        .and_then(|(s, _)| self.schemas.get(s).cloned());
                                    let selected_cols = call
                                        .arguments
                                        .args
                                        .first()
                                        .and_then(Self::extract_string_list);
                                    match selected_cols {
                                        Some(cols) => {
                                            if let Some(ref bc) = base_cols {
                                                for col in &cols {
                                                    if !bc.contains(col) {
                                                        let schema_display = base_info
                                                            .as_ref()
                                                            .map(|(s, l)| {
                                                                self.schema_display(s, *l)
                                                            })
                                                            .unwrap_or_else(|| {
                                                                "unknown".to_string()
                                                            });
                                                        errors.push(LintError {
                                                            line: current_line,
                                                            col: current_col,
                                                            code: CODE_UNKNOWN_COLUMN.to_string(),
                                                            message: format!(
                                                                "Column '{}' does not exist in {}",
                                                                col, schema_display
                                                            ),
                                                            severity: "error".to_string(),
                                                        });
                                                    }
                                                }
                                            }
                                            let target_names: Vec<String> = assign
                                                .targets
                                                .iter()
                                                .filter_map(|t| {
                                                    if let Expr::Name(n) = t {
                                                        Some(n.id.to_string())
                                                    } else {
                                                        None
                                                    }
                                                })
                                                .collect();
                                            let var_name = target_names
                                                .first()
                                                .map(|s| s.as_str())
                                                .unwrap_or("unknown");
                                            let schema_name = self.make_inferred_schema(
                                                cols,
                                                var_name,
                                                current_line,
                                            );
                                            for name in &target_names {
                                                self.variables.insert(
                                                    name.clone(),
                                                    (schema_name.clone(), current_line),
                                                );
                                            }
                                        }
                                        None => {
                                            if let Some((base_schema, _)) = base_info {
                                                for target in &assign.targets {
                                                    if let Expr::Name(target_name) = target {
                                                        self.variables.insert(
                                                            target_name.id.to_string(),
                                                            (base_schema.clone(), current_line),
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            } else if FEAST_RETRIEVAL_METHODS.contains(&func_name) {
                                // job = store.get_historical_features(entity_df=..., features=[...])
                                // — the split form's first half. Not yet a DataFrame (a
                                // RetrievalJob/OnlineResponse), so tracked in the
                                // separate `retrieval_jobs` map rather than
                                // `self.variables` — otherwise `job["x"]` before
                                // `.to_df()` would be (wrongly) validated as a column
                                // access. See `register_feast_dataframe` for where this
                                // actually becomes a tracked DataFrame.
                                if let Some(cols) = self.extract_feast_feature_columns(call) {
                                    if let [Expr::Name(target_name)] = assign.targets.as_slice() {
                                        self.retrieval_jobs
                                            .insert(target_name.id.to_string(), cols);
                                    }
                                }
                            } else if func_name == "to_df" {
                                // Either half of Feast's two DataFrame-materializing
                                // shapes: the split form's second half
                                // (`df = job.to_df()`, `job` resolved via
                                // `retrieval_jobs` above), or the chained form
                                // (`df = store.get_historical_features(...).to_df()`)
                                // in one statement. Anything else calling `.to_df()`
                                // (unrelated to Feast) is deliberately left alone —
                                // matched only once one of these two specific shapes is
                                // confirmed, not on the method name alone.
                                let feast_cols = match &*attr.value {
                                    Expr::Name(recv) => {
                                        self.retrieval_jobs.get(recv.id.as_str()).cloned().map(Some)
                                    }
                                    Expr::Call(inner_call) => match &*inner_call.func {
                                        Expr::Attribute(inner_attr)
                                            if FEAST_RETRIEVAL_METHODS
                                                .contains(&inner_attr.attr.as_str()) =>
                                        {
                                            Some(self.extract_feast_feature_columns(inner_call))
                                        }
                                        _ => None,
                                    },
                                    _ => None,
                                };
                                if let Some(cols) = feast_cols {
                                    let target_names: Vec<String> = assign
                                        .targets
                                        .iter()
                                        .filter_map(|t| {
                                            if let Expr::Name(n) = t {
                                                Some(n.id.to_string())
                                            } else {
                                                None
                                            }
                                        })
                                        .collect();
                                    let var_name =
                                        target_names.first().map(|s| s.as_str()).unwrap_or("df");
                                    self.register_feast_dataframe(
                                        cols,
                                        &target_names,
                                        var_name,
                                        current_line,
                                        current_col,
                                        errors,
                                    );
                                }
                            } else if SQL_FINALIZE_METHODS.contains(&func_name) {
                                // client.query(sql).to_dataframe() / spark.sql(sql)
                                // .toPandas() / duckdb.sql(sql).df()/.pl() — only
                                // dispatched once the call this is chained onto is
                                // confirmed to be one of SQL_PRODUCING_METHODS with a
                                // resolvable SQL argument; `.df()`/`.pl()`/`.toPandas()`
                                // alone are too generic a signal (plenty of unrelated
                                // code has methods by those names) to count or warn on
                                // by name alone.
                                if let Some(inner_call) = Self::sql_producing_call(&attr.value) {
                                    let sql = self.extract_sql_literal(inner_call);
                                    let target_names: Vec<String> = assign
                                        .targets
                                        .iter()
                                        .filter_map(|t| {
                                            if let Expr::Name(n) = t {
                                                Some(n.id.to_string())
                                            } else {
                                                None
                                            }
                                        })
                                        .collect();
                                    let var_name =
                                        target_names.first().map(|s| s.as_str()).unwrap_or("df");
                                    self.register_sql_dataframe(
                                        sql.as_deref(),
                                        &target_names,
                                        var_name,
                                        current_line,
                                        current_col,
                                        errors,
                                    );
                                }
                            } else if func_name == "fetch_pandas_all" {
                                // cursor.fetch_pandas_all() — the second half of the
                                // Snowflake/Redshift cursor pattern; the SQL text was
                                // recorded by the `cursor.execute(sql)` bare-statement
                                // handling above. Unlike the finalize methods above,
                                // this name is specific enough that dispatching on it
                                // alone (even with no prior tracked `execute()`) is
                                // safe — it isn't a generic accessor name plausibly
                                // used for something unrelated.
                                if let Expr::Name(recv) = &*attr.value {
                                    let sql = self.cursor_sql.get(recv.id.as_str()).cloned();
                                    let target_names: Vec<String> = assign
                                        .targets
                                        .iter()
                                        .filter_map(|t| {
                                            if let Expr::Name(n) = t {
                                                Some(n.id.to_string())
                                            } else {
                                                None
                                            }
                                        })
                                        .collect();
                                    let var_name =
                                        target_names.first().map(|s| s.as_str()).unwrap_or("df");
                                    self.register_sql_dataframe(
                                        sql.as_deref(),
                                        &target_names,
                                        var_name,
                                        current_line,
                                        current_col,
                                        errors,
                                    );
                                }
                            } else if func_name == "drop" {
                                if let Expr::Name(recv) = &*attr.value {
                                    let recv_str = recv.id.as_str();
                                    let base_info =
                                        self.variables.get(recv_str).map(|(s, l)| (s.clone(), *l));
                                    let base_cols = base_info
                                        .as_ref()
                                        .and_then(|(s, _)| self.schemas.get(s).cloned());
                                    let dropped = Self::extract_drop_columns(call);
                                    match (base_cols, dropped) {
                                        (Some(base_cols), Some(dropped_cols)) => {
                                            for col in &dropped_cols {
                                                if !base_cols.contains(col) {
                                                    let schema_display = base_info
                                                        .as_ref()
                                                        .map(|(s, l)| self.schema_display(s, *l))
                                                        .unwrap_or_else(|| "unknown".to_string());
                                                    errors.push(LintError {
                                                        line: current_line,
                                                        col: current_col,
                                                        code: CODE_DROPPED_UNKNOWN_COLUMN.to_string(),
                                                        message: format!(
                                                            "Dropped column '{}' does not exist in {}",
                                                            col, schema_display
                                                        ),
                                                        severity: "warning".to_string(),
                                                    });
                                                }
                                            }
                                            let new_cols: Vec<String> = base_cols
                                                .into_iter()
                                                .filter(|c| !dropped_cols.contains(c))
                                                .collect();
                                            let target_names: Vec<String> = assign
                                                .targets
                                                .iter()
                                                .filter_map(|t| {
                                                    if let Expr::Name(n) = t {
                                                        Some(n.id.to_string())
                                                    } else {
                                                        None
                                                    }
                                                })
                                                .collect();
                                            let var_name = target_names
                                                .first()
                                                .map(|s| s.as_str())
                                                .unwrap_or("unknown");
                                            let schema_name = self.make_inferred_schema(
                                                new_cols,
                                                var_name,
                                                current_line,
                                            );
                                            for name in &target_names {
                                                self.variables.insert(
                                                    name.clone(),
                                                    (schema_name.clone(), current_line),
                                                );
                                            }
                                        }
                                        _ => {
                                            // Can't extract cols or no base — passthrough base
                                            if let Some((base_schema, _)) = base_info {
                                                for target in &assign.targets {
                                                    if let Expr::Name(target_name) = target {
                                                        self.variables.insert(
                                                            target_name.id.to_string(),
                                                            (base_schema.clone(), current_line),
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            } else if func_name == "rename" {
                                if let Expr::Name(recv) = &*attr.value {
                                    let recv_str = recv.id.as_str();
                                    let base_info =
                                        self.variables.get(recv_str).map(|(s, l)| (s.clone(), *l));
                                    let base_cols = base_info
                                        .as_ref()
                                        .and_then(|(s, _)| self.schemas.get(s).cloned());
                                    let mapping = Self::extract_rename_mapping(call);
                                    let case_fold = if mapping.is_none() {
                                        Self::extract_rename_case_fold(call)
                                    } else {
                                        None
                                    };
                                    match (base_cols, mapping, case_fold) {
                                        (Some(base_cols), None, Some(fold)) => {
                                            let new_cols: Vec<String> =
                                                base_cols.iter().map(|c| fold.apply(c)).collect();
                                            let target_names: Vec<String> = assign
                                                .targets
                                                .iter()
                                                .filter_map(|t| {
                                                    if let Expr::Name(n) = t {
                                                        Some(n.id.to_string())
                                                    } else {
                                                        None
                                                    }
                                                })
                                                .collect();
                                            let var_name = target_names
                                                .first()
                                                .map(|s| s.as_str())
                                                .unwrap_or("unknown");
                                            let schema_name = self.make_inferred_schema(
                                                new_cols,
                                                var_name,
                                                current_line,
                                            );
                                            for name in &target_names {
                                                self.variables.insert(
                                                    name.clone(),
                                                    (schema_name.clone(), current_line),
                                                );
                                            }
                                        }
                                        (Some(base_cols), Some(mapping), _) => {
                                            let schema_display = base_info
                                                .as_ref()
                                                .map(|(s, l)| self.schema_display(s, *l))
                                                .unwrap_or_else(|| "unknown".to_string());
                                            for old_col in mapping.keys() {
                                                if !base_cols.contains(old_col) {
                                                    errors.push(LintError {
                                                        line: current_line,
                                                        col: current_col,
                                                        code: CODE_UNKNOWN_COLUMN.to_string(),
                                                        message: format!(
                                                            "Column '{}' does not exist in {} (rename)",
                                                            old_col, schema_display
                                                        ),
                                                        severity: "error".to_string(),
                                                    });
                                                }
                                            }
                                            let new_cols: Vec<String> = base_cols
                                                .iter()
                                                .map(|c| {
                                                    mapping
                                                        .get(c)
                                                        .cloned()
                                                        .unwrap_or_else(|| c.clone())
                                                })
                                                .collect();
                                            let target_names: Vec<String> = assign
                                                .targets
                                                .iter()
                                                .filter_map(|t| {
                                                    if let Expr::Name(n) = t {
                                                        Some(n.id.to_string())
                                                    } else {
                                                        None
                                                    }
                                                })
                                                .collect();
                                            let var_name = target_names
                                                .first()
                                                .map(|s| s.as_str())
                                                .unwrap_or("unknown");
                                            let schema_name = self.make_inferred_schema(
                                                new_cols,
                                                var_name,
                                                current_line,
                                            );
                                            for name in &target_names {
                                                self.variables.insert(
                                                    name.clone(),
                                                    (schema_name.clone(), current_line),
                                                );
                                            }
                                        }
                                        _ => {
                                            if let Some((base_schema, _)) = base_info {
                                                for target in &assign.targets {
                                                    if let Expr::Name(target_name) = target {
                                                        self.variables.insert(
                                                            target_name.id.to_string(),
                                                            (base_schema.clone(), current_line),
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            } else if func_name == "assign" {
                                if let Expr::Name(recv) = &*attr.value {
                                    let recv_str = recv.id.as_str();
                                    let base_info =
                                        self.variables.get(recv_str).map(|(s, _)| s.clone());
                                    let mut new_cols: Vec<String> = base_info
                                        .as_ref()
                                        .and_then(|s| self.schemas.get(s).cloned())
                                        .unwrap_or_default();
                                    for keyword in &call.arguments.keywords {
                                        if let Some(kw_name) =
                                            keyword.arg.as_ref().map(|s| s.as_str())
                                        {
                                            if !new_cols.contains(&kw_name.to_string()) {
                                                new_cols.push(kw_name.to_string());
                                            }
                                        }
                                    }
                                    let target_names: Vec<String> = assign
                                        .targets
                                        .iter()
                                        .filter_map(|t| {
                                            if let Expr::Name(n) = t {
                                                Some(n.id.to_string())
                                            } else {
                                                None
                                            }
                                        })
                                        .collect();
                                    let var_name = target_names
                                        .first()
                                        .map(|s| s.as_str())
                                        .unwrap_or("unknown");
                                    let schema_name =
                                        self.make_inferred_schema(new_cols, var_name, current_line);
                                    for name in &target_names {
                                        self.variables.insert(
                                            name.clone(),
                                            (schema_name.clone(), current_line),
                                        );
                                    }
                                }
                            } else if func_name == "pop" {
                                // pop('col') removes a column in-place and returns a Series.
                                // Mutate the receiver's schema; do not track the assignment target.
                                if let Expr::Name(recv) = &*attr.value {
                                    if let Some(col_name) = call
                                        .arguments
                                        .args
                                        .first()
                                        .and_then(|a| Self::extract_string_literal(a))
                                    {
                                        self.remove_column_inplace(
                                            recv.id.as_str(),
                                            col_name,
                                            current_line,
                                            current_col,
                                            "pop",
                                            errors,
                                        );
                                    }
                                }
                            } else if func_name == "insert" {
                                // insert(loc, col, value) adds a column in-place; returns None.
                                // Mutate the receiver's schema; do not track the assignment target.
                                if let Expr::Name(recv) = &*attr.value {
                                    if let Some(col_name) = call
                                        .arguments
                                        .args
                                        .get(1)
                                        .and_then(|a| Self::extract_string_literal(a))
                                    {
                                        self.add_column_inplace(
                                            recv.id.as_str(),
                                            col_name,
                                            current_line,
                                        );
                                    }
                                }
                            }
                            // Validate pl.col() / col() references for any method call on a tracked variable.
                            if let Expr::Name(recv) = &*attr.value {
                                self.validate_pl_col_args_on_receiver(
                                    recv.id.as_str(),
                                    call,
                                    current_line,
                                    current_col,
                                    errors,
                                );
                            }
                        }
                        Expr::Name(name) if name.id.as_str() == "concat" => {
                            if !call.arguments.args.is_empty() {
                                if let Expr::List(list) = &call.arguments.args[0] {
                                    let mut schemas = Vec::new();
                                    for el in &list.elts {
                                        if let Expr::Name(n) = el {
                                            if let Some((s, _)) = self.variables.get(n.id.as_str())
                                            {
                                                schemas.push(s.clone());
                                            }
                                        }
                                    }
                                    if schemas.len() >= 2 {
                                        is_merge_or_concat = true;
                                        merge_schema =
                                            Some((schemas[0].clone(), schemas[1].clone()));
                                    }
                                }
                            } else if let Some(keyword) = call
                                .arguments
                                .keywords
                                .iter()
                                .find(|k| k.arg.as_ref().map(|s| s.as_str()) == Some("objs"))
                            {
                                if let Expr::List(list) = &keyword.value {
                                    let mut schemas = Vec::new();
                                    for el in &list.elts {
                                        if let Expr::Name(n) = el {
                                            if let Some((s, _)) = self.variables.get(n.id.as_str())
                                            {
                                                schemas.push(s.clone());
                                            }
                                        }
                                    }
                                    if schemas.len() >= 2 {
                                        is_merge_or_concat = true;
                                        merge_schema =
                                            Some((schemas[0].clone(), schemas[1].clone()));
                                    }
                                }
                            }
                        }
                        _ => {}
                    }

                    if is_merge_or_concat {
                        if let Some((s1, s2)) = merge_schema {
                            // Union semantics: the result of merge/concat contains every
                            // column from both inputs.  Sort + dedup gives a stable,
                            // canonical column order and eliminates duplicates that arise
                            // when both DataFrames share key columns (e.g. a join key).
                            let mut combined_cols = Vec::new();
                            if let Some(cols1) = self.schemas.get(&s1) {
                                combined_cols.extend(cols1.clone());
                            }
                            if let Some(cols2) = self.schemas.get(&s2) {
                                combined_cols.extend(cols2.clone());
                            }
                            combined_cols.sort();
                            combined_cols.dedup();

                            let combined_schema_name = format!("{}_{}", s1, s2);
                            // An open schema (see `register_feast_dataframe`) stays open
                            // after a merge/concat: its column list was already known to
                            // be incomplete before the join, and combining it with
                            // another frame's columns doesn't make it any more complete.
                            if self.open_schemas.contains(&s1) || self.open_schemas.contains(&s2) {
                                self.open_schemas.insert(combined_schema_name.clone());
                            }
                            self.schemas
                                .insert(combined_schema_name.clone(), combined_cols);
                            for target in &assign.targets {
                                if let Expr::Name(target_name) = target {
                                    self.variables.insert(
                                        target_name.id.to_string(),
                                        (combined_schema_name.clone(), current_line),
                                    );
                                }
                            }
                        }
                    }

                    // Support for DataFrame[Schema](...) instantiation
                    if let Expr::Subscript(subscript) = &*call.func {
                        if let Expr::Name(name) = &*subscript.value {
                            let type_name = name.id.as_str();
                            if type_name == "DataFrame"
                                || type_name == "PandasFrame"
                                || type_name == "PolarsFrame"
                            {
                                if let Expr::Name(schema_name) = &*subscript.slice {
                                    for target in &assign.targets {
                                        if let Expr::Name(target_name) = target {
                                            self.variables.insert(
                                                target_name.id.to_string(),
                                                (schema_name.id.to_string(), current_line),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    } else if let Expr::Attribute(attr) = &*call.func {
                        // Handle Schema().read_csv(...) style
                        let current_expr = &*attr.value;
                        if let Expr::Call(inner_call) = current_expr {
                            if let Expr::Name(schema_name) = &*inner_call.func {
                                if self.schemas.contains_key(schema_name.id.as_str()) {
                                    for target in &assign.targets {
                                        if let Expr::Name(target_name) = target {
                                            self.variables.insert(
                                                target_name.id.to_string(),
                                                (schema_name.id.to_string(), current_line),
                                            );
                                        }
                                    }
                                }
                            }
                        } else if let Expr::Name(base) = current_expr {
                            // Handle result = module.trim_customers(customers) — an
                            // attribute-style call to a delegate reached via a plain
                            // `import module` (see load_cross_file_symbols, which
                            // populates self.param_requires by bare function name
                            // regardless of how it's called at the use site). Guarded
                            // on `base` not being a tracked DataFrame variable so a
                            // genuine method call (`df.merge(other)`) is never treated
                            // as a call to a same-named cross-file delegate function.
                            if !self.variables.contains_key(base.id.as_str()) {
                                self.check_call_requirements(
                                    attr.attr.as_str(),
                                    call,
                                    current_line,
                                    current_col,
                                    errors,
                                );
                            }
                        }
                    } else if let Expr::Name(func_name) = &*call.func {
                        // Handle df = load_users() where load_users() -> PandasFrame[Schema]
                        if let Some(schema_name) = self.functions.get(func_name.id.as_str()) {
                            let schema_name = schema_name.clone();
                            self.dataframes_total += 1;
                            self.dataframes_typed += 1;
                            for target in &assign.targets {
                                if let Expr::Name(target_name) = target {
                                    self.variables.insert(
                                        target_name.id.to_string(),
                                        (schema_name.clone(), current_line),
                                    );
                                }
                            }
                        }
                        // Validate the call's first argument against the callee's
                        // parameter contract, e.g. `trimmed = trim_customers(customers)`.
                        self.check_call_requirements(
                            func_name.id.as_str(),
                            call,
                            current_line,
                            current_col,
                            errors,
                        );
                    }
                }
                for target in &assign.targets {
                    self.visit_expr(target, errors);
                }
                self.visit_expr(&assign.value, errors);
            }
            Stmt::AnnAssign(ann_assign) => {
                let (current_line, _) = self.source_location(ann_assign.range().start());

                if let Some(value) = &ann_assign.value {
                    if let Expr::Call(call) = &**value {
                        if let Expr::Subscript(subscript) = &*call.func {
                            if let Expr::Name(name) = &*subscript.value {
                                let type_name = name.id.as_str();
                                if type_name == "DataFrame"
                                    || type_name == "PandasFrame"
                                    || type_name == "PolarsFrame"
                                {
                                    if let Expr::Name(schema_name) = &*subscript.slice {
                                        if let Expr::Name(target_name) = &*ann_assign.target {
                                            self.variables.insert(
                                                target_name.id.to_string(),
                                                (schema_name.id.to_string(), current_line),
                                            );
                                        }
                                    }
                                }
                            }
                        } else if let Expr::Attribute(attr) = &*call.func {
                            let current_expr = &*attr.value;
                            if let Expr::Call(inner_call) = current_expr {
                                if let Expr::Name(schema_name) = &*inner_call.func {
                                    if self.schemas.contains_key(schema_name.id.as_str()) {
                                        if let Expr::Name(target_name) = &*ann_assign.target {
                                            self.variables.insert(
                                                target_name.id.to_string(),
                                                (schema_name.id.to_string(), current_line),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // Track schema from type annotation
                match &*ann_assign.annotation {
                    Expr::Subscript(subscript) => {
                        let mut type_name = None;
                        if let Expr::Name(name) = &*subscript.value {
                            type_name = Some(name.id.as_str());
                        } else if let Expr::Attribute(attr) = &*subscript.value {
                            type_name = Some(attr.attr.as_str());
                        }

                        if let Some(name) = type_name {
                            // DataFrame[Schema], PandasFrame[Schema], PolarsFrame[Schema]
                            if name == "DataFrame" || name == "PandasFrame" || name == "PolarsFrame"
                            {
                                if let Expr::Name(schema_name) = &*subscript.slice {
                                    if let Expr::Name(target_name) = &*ann_assign.target {
                                        self.variables.insert(
                                            target_name.id.to_string(),
                                            (schema_name.id.to_string(), current_line),
                                        );
                                    }
                                }
                            } else if name == "Annotated" {
                                // Annotated[DataFrame, Schema] or Annotated[pl.DataFrame, Schema]
                                if let Expr::Tuple(tuple) = &*subscript.slice {
                                    if tuple.elts.len() >= 2 {
                                        let mut is_dataframe = false;
                                        if let Expr::Name(first) = &tuple.elts[0] {
                                            let first_name = first.id.as_str();
                                            if first_name == "DataFrame"
                                                || first_name.contains("DataFrame")
                                            {
                                                is_dataframe = true;
                                            }
                                        } else if let Expr::Attribute(first_attr) = &tuple.elts[0] {
                                            if first_attr.attr.as_str() == "DataFrame" {
                                                is_dataframe = true;
                                            }
                                        }
                                        if is_dataframe {
                                            if let Expr::Name(schema_name) = &tuple.elts[1] {
                                                if let Expr::Name(target_name) = &*ann_assign.target
                                                {
                                                    self.variables.insert(
                                                        target_name.id.to_string(),
                                                        (schema_name.id.to_string(), current_line),
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Expr::StringLiteral(s) => {
                        // Handle quoted type hints: df: "DataFrame[UserSchema]"
                        self.parse_quoted_type_hint(s.value.to_str(), ann_assign, current_line);
                    }
                    _ => {}
                }

                self.visit_expr(&ann_assign.target, errors);
                if let Some(value) = &ann_assign.value {
                    self.visit_expr(value, errors);
                }
            }
            Stmt::Expr(expr_stmt) => {
                // Intercept in-place mutations before generic expression visiting.
                if let Expr::Call(call) = &*expr_stmt.value {
                    if let Expr::Attribute(attr) = &*call.func {
                        let func_name = attr.attr.as_str();
                        let (line, col) = self.source_location(call.range().start());
                        if func_name == "pop" {
                            if let Expr::Name(recv) = &*attr.value {
                                if let Some(col_name) = call
                                    .arguments
                                    .args
                                    .first()
                                    .and_then(|a| Self::extract_string_literal(a))
                                {
                                    self.remove_column_inplace(
                                        recv.id.as_str(),
                                        col_name,
                                        line,
                                        col,
                                        "pop",
                                        errors,
                                    );
                                }
                            }
                        } else if func_name == "insert" {
                            if let Expr::Name(recv) = &*attr.value {
                                if let Some(col_name) = call
                                    .arguments
                                    .args
                                    .get(1)
                                    .and_then(|a| Self::extract_string_literal(a))
                                {
                                    self.add_column_inplace(recv.id.as_str(), col_name, line);
                                }
                            }
                        } else if func_name == "execute" {
                            // cursor.execute(sql) — the PEP 249 pattern used by
                            // Snowflake, Redshift, and similar DB-API connectors. The
                            // SQL text is bound to the cursor, not returned, so it's
                            // tracked in `cursor_sql` until a later
                            // `cursor.fetch_pandas_all()` (see the Assign arm) turns it
                            // into a DataFrame.
                            if let Expr::Name(recv) = &*attr.value {
                                let sql = call
                                    .arguments
                                    .args
                                    .first()
                                    .and_then(|a| self.extract_sql_expr(a));
                                match sql {
                                    Some(sql) => {
                                        self.cursor_sql.insert(recv.id.to_string(), sql);
                                    }
                                    None => {
                                        self.cursor_sql.remove(recv.id.as_str());
                                    }
                                }
                            }
                        }
                        // Validate pl.col() / col() references for bare expression method calls.
                        if let Expr::Name(recv) = &*attr.value {
                            self.validate_pl_col_args_on_receiver(
                                recv.id.as_str(),
                                call,
                                line,
                                col,
                                errors,
                            );
                            // Bare call statement via a plain `import module`, e.g.
                            // `transforms.trim_customers(customers)` — guarded on `recv`
                            // not being a tracked DataFrame variable so a genuine method
                            // call is never treated as a call to a same-named delegate.
                            if !self.variables.contains_key(recv.id.as_str()) {
                                self.check_call_requirements(func_name, call, line, col, errors);
                            }
                        }
                    } else if let Expr::Name(func_name) = &*call.func {
                        // Bare call statement, e.g. `trim_customers(customers)` with no
                        // assignment — still validate against the callee's parameter contract.
                        let (line, col) = self.source_location(call.range().start());
                        self.check_call_requirements(
                            func_name.id.as_str(),
                            call,
                            line,
                            col,
                            errors,
                        );
                    }
                }
                self.visit_expr(&expr_stmt.value, errors);
            }
            Stmt::Delete(delete) => {
                for target in &delete.targets {
                    if let Expr::Subscript(subscript) = target {
                        if let Expr::Name(recv) = &*subscript.value {
                            if let Some(col_name) = Self::extract_string_literal(&subscript.slice) {
                                let (line, col) = self.source_location(subscript.range().start());
                                self.remove_column_inplace(
                                    recv.id.as_str(),
                                    col_name,
                                    line,
                                    col,
                                    "del",
                                    errors,
                                );
                            }
                        }
                    }
                }
            }
            // `return <expr>` was never dispatched to visit_expr at all — a completely
            // separate gap from the BinOp/keyword-arg recursion fixed in visit_expr itself.
            // Any column access whose only appearance is in a return statement (extremely
            // common for small helper functions, e.g. `return df["a"] + df["bad"]`) was
            // silently invisible to validation.
            Stmt::Return(ret) => {
                if let Some(value) = &ret.value {
                    self.visit_expr(value, errors);
                }
            }
            _ => {}
        }
    }

    fn parse_quoted_type_hint(
        &mut self,
        s: &str,
        ann_assign: &ast::StmtAnnAssign,
        current_line: usize,
    ) {
        // Handle patterns like "DataFrame[Schema]", "PandasFrame[Schema]", "PolarsFrame[Schema]"
        // and "Annotated[DataFrame, Schema]", "Annotated[pl.DataFrame, Schema]"

        let patterns = ["DataFrame[", "PandasFrame[", "PolarsFrame["];
        for pattern in patterns {
            if s.contains(pattern) {
                if let Some(start) = s.find('[') {
                    if let Some(end) = s.rfind(']') {
                        let schema_name = &s[start + 1..end];
                        // Handle nested generics by taking the last part
                        let schema = schema_name
                            .split(',')
                            .next_back()
                            .unwrap_or(schema_name)
                            .trim();
                        if let Expr::Name(target_name) = &*ann_assign.target {
                            self.variables.insert(
                                target_name.id.to_string(),
                                (schema.to_string(), current_line),
                            );
                        }
                    }
                }
                return;
            }
        }

        // Handle Annotated pattern
        if s.contains("Annotated[") && s.contains("DataFrame") {
            // Extract schema from Annotated[DataFrame, Schema] or Annotated[pl.DataFrame, Schema]
            if let Some(start) = s.find("Annotated[") {
                let inner = &s[start + 10..]; // Skip "Annotated["
                if let Some(end) = inner.rfind(']') {
                    let parts: Vec<&str> = inner[..end].split(',').collect();
                    if parts.len() >= 2 {
                        let schema = parts[1].trim();
                        if let Expr::Name(target_name) = &*ann_assign.target {
                            self.variables.insert(
                                target_name.id.to_string(),
                                (schema.to_string(), current_line),
                            );
                        }
                    }
                }
            }
        }
    }

    // Validate column access expressions against known schemas.
    //
    // Checked expression kinds:
    //
    // * `Attribute` (`df.col_name`) — validates `col_name` against the schema of `df`
    //   if `df` is a tracked variable, skipping names in `RESERVED_METHODS`.
    // * `Subscript` (`df["col_name"]`) — validates the string literal key.
    // * `Call` — recurses into positional arguments and, when the callee is
    //   `receiver.method(...)`, recurses only into `receiver` rather than the method
    //   name itself.  This avoids false positives where the method name (e.g. `assign`,
    //   `groupby`) is mistakenly checked as a column.
    //
    // Typo suggestions are added via find_best_match when the edit distance to the
    // closest known column name is ≤ 2.
    fn visit_expr(&self, expr: &Expr, errors: &mut Vec<LintError>) {
        match expr {
            Expr::Attribute(attr) => {
                if let Expr::Name(name) = &*attr.value {
                    if let Some((schema_name, defined_line)) = self.variables.get(name.id.as_str())
                    {
                        if let Some(columns) = self.schemas.get(schema_name) {
                            let attr_name = attr.attr.as_str();
                            if !self.schema_has_column(schema_name, attr_name)
                                && !RESERVED_METHODS.contains(&attr_name)
                            {
                                let (line, col) = self.source_location(attr.range().start());
                                let schema_display =
                                    self.schema_display(schema_name, *defined_line);
                                let mut message = format!(
                                    "Column '{}' does not exist in {}",
                                    attr_name, schema_display
                                );
                                if let Some(suggestion) = find_best_match(attr_name, columns) {
                                    message.push_str(&format!(" (did you mean '{}'?)", suggestion));
                                }
                                errors.push(LintError {
                                    line,
                                    col,
                                    code: CODE_UNKNOWN_COLUMN.to_string(),
                                    message,
                                    severity: "error".to_string(),
                                });
                            }
                        }
                    }
                }
                self.visit_expr(&attr.value, errors);
            }
            Expr::Subscript(subscript) => {
                if let Expr::Name(name) = &*subscript.value {
                    if let Some((schema_name, defined_line)) = self.variables.get(name.id.as_str())
                    {
                        if let Some(columns) = self.schemas.get(schema_name) {
                            if let Some(col_name) = Self::extract_string_literal(&subscript.slice) {
                                if !self.schema_has_column(schema_name, col_name) {
                                    let (line, col) =
                                        self.source_location(subscript.range().start());
                                    let schema_display =
                                        self.schema_display(schema_name, *defined_line);
                                    let mut message = format!(
                                        "Column '{}' does not exist in {}",
                                        col_name, schema_display
                                    );
                                    if let Some(suggestion) = find_best_match(col_name, columns) {
                                        message.push_str(&format!(
                                            " (did you mean '{}'?)",
                                            suggestion
                                        ));
                                    }
                                    errors.push(LintError {
                                        line,
                                        col,
                                        code: CODE_UNKNOWN_COLUMN.to_string(),
                                        message,
                                        severity: "error".to_string(),
                                    });
                                }
                            }
                        }
                    }
                }
                self.visit_expr(&subscript.value, errors);
                self.visit_expr(&subscript.slice, errors);
            }
            Expr::Call(call) => {
                for arg in call.arguments.args.iter() {
                    self.visit_expr(arg, errors);
                }
                // Keyword arguments (e.g. `.assign(amount_vat=public["amount"] * 1.2)`)
                // carry column accesses just as often as positional ones — must be
                // checked too, not just skipped as "the method name".
                for kw in call.arguments.keywords.iter() {
                    self.visit_expr(&kw.value, errors);
                }
                // When the callee is `receiver.method(...)`, do not check the method name
                // as a column access — only recurse into the receiver so that any column
                // accesses nested there (e.g. `df.col.method()`) are still found.
                if let Expr::Attribute(attr) = &*call.func {
                    self.visit_expr(&attr.value, errors);
                } else {
                    self.visit_expr(&call.func, errors);
                }
            }
            // Column accesses nested inside arithmetic, boolean, comparison, or
            // literal-collection expressions (`df["a"] + df["b"]`, `df["x"] > 0`,
            // `[df["a"], df["b"]]`, …) must still be recursed into — otherwise any
            // access wrapped in one of these silently escapes validation entirely,
            // regardless of whether it's schema-based, inferred, or contract-based.
            Expr::BinOp(binop) => {
                self.visit_expr(&binop.left, errors);
                self.visit_expr(&binop.right, errors);
            }
            Expr::BoolOp(boolop) => {
                for v in &boolop.values {
                    self.visit_expr(v, errors);
                }
            }
            Expr::UnaryOp(unary) => {
                self.visit_expr(&unary.operand, errors);
            }
            Expr::Compare(compare) => {
                self.visit_expr(&compare.left, errors);
                for comp in compare.comparators.iter() {
                    self.visit_expr(comp, errors);
                }
            }
            Expr::List(list) => {
                for el in &list.elts {
                    self.visit_expr(el, errors);
                }
            }
            Expr::Tuple(tuple) => {
                for el in &tuple.elts {
                    self.visit_expr(el, errors);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_compute_levenshtein_distance() {
        // arrange
        let a = "email";
        let b = "emai";

        // act
        let dist = levenshtein(a, b);

        // assert
        assert_eq!(dist, 1);
    }

    #[test]
    fn test_should_find_best_match_for_typo() {
        // arrange
        let name = "emai";
        let candidates = vec!["user_id".to_string(), "email".to_string()];

        // act
        let result = find_best_match(name, &candidates);

        // assert
        assert_eq!(result, Some("email"));
    }

    #[test]
    fn test_should_detect_base_schema_class() {
        // arrange/act/assert
        assert!(Linter::is_schema_base("BaseSchema"));
        assert!(Linter::is_schema_base("DataFrameModel"));
        assert!(Linter::is_schema_base("DataFrame"));
        assert!(Linter::is_schema_base("BaseFrame"));
        assert!(!Linter::is_schema_base("SomeOtherClass"));
    }

    #[test]
    fn test_should_lint_base_schema_column_access() {
        // arrange
        let source = r#"
from typedframes import BaseSchema, Column

class UserSchema(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)

df: DataFrame[UserSchema] = load()
print(df["user_id"])
print(df["name"])
"#;
        let mut linter = Linter::new();

        // act
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        // assert
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("name"));
        assert!(errors[0].message.contains("UserSchema"));
    }

    #[test]
    fn test_should_lint_annotated_polars_pattern() {
        // arrange
        let source = r#"
from typing import Annotated
import polars as pl
from typedframes import BaseSchema, Column

class UserSchema(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)

df: Annotated[pl.DataFrame, UserSchema] = pl.read_csv("data.csv")
print(df["user_id"])
print(df["wrong_column"])
"#;
        let mut linter = Linter::new();

        // act
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        // assert
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("wrong_column"));
        assert!(errors[0].message.contains("UserSchema"));
    }

    #[test]
    fn test_should_track_function_return_type() {
        // arrange
        let source = r#"
from typedframes import BaseSchema, Column
from typedframes.pandas import PandasFrame

class UserSchema(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)

def load_users() -> PandasFrame[UserSchema]:
    return PandasFrame.from_schema(pd.read_csv("users.csv"), UserSchema)

df = load_users()
print(df["user_id"])
print(df["name"])
print(df["emai"])
"#;
        let mut linter = Linter::new();

        // act
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        // assert
        assert_eq!(errors.len(), 2);
        assert!(errors[0].message.contains("name"));
        assert!(errors[0].message.contains("UserSchema"));
        assert!(errors[1].message.contains("emai"));
        assert!(errors[1].message.contains("did you mean 'email'"));
    }

    #[test]
    fn test_should_count_typed_dataframe_for_load_call_with_usecols() {
        // arrange
        let source = r#"
import pandas as pd

df = pd.read_csv("data.csv", usecols=["a", "b"])
print(df["a"])
"#;
        let mut linter = Linter::new();

        // act
        linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        // assert
        assert_eq!(linter.dataframes_total, 1);
        assert_eq!(linter.dataframes_typed, 1);
    }

    #[test]
    fn test_should_count_untyped_dataframe_for_bare_load_call() {
        // arrange: no usecols/columns, so the checker has no column information
        let source = r#"
import pandas as pd

df = pd.read_csv("data.csv")
print(df["a"])
"#;
        let mut linter = Linter::new();

        // act
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        // assert: counted as seen, but not typed — and still raises the existing warning
        assert_eq!(linter.dataframes_total, 1);
        assert_eq!(linter.dataframes_typed, 0);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, "untracked-dataframe");
    }

    #[test]
    fn test_should_count_typed_dataframe_for_schema_from_pandas() {
        // arrange
        let source = r#"
import pandas as pd
from typedframes import BaseSchema, Column

class UserSchema(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)

raw = pd.read_csv("users.csv")
df = UserSchema.from_pandas(raw)
print(df["user_id"])
"#;
        let mut linter = Linter::new();

        // act
        linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        // assert: the bare `raw = pd.read_csv(...)` load call is untyped, but the
        // `UserSchema.from_pandas(raw)` assignment is a second, typed DataFrame origin
        assert_eq!(linter.dataframes_total, 2);
        assert_eq!(linter.dataframes_typed, 1);
    }

    #[test]
    fn test_should_count_typed_dataframe_for_function_return_schema() {
        // arrange
        let source = r#"
from typedframes import BaseSchema, Column
from typedframes.pandas import PandasFrame

class UserSchema(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)

def load_users() -> PandasFrame[UserSchema]:
    return PandasFrame.from_schema(pd.read_csv("users.csv"), UserSchema)

df = load_users()
print(df["user_id"])
"#;
        let mut linter = Linter::new();

        // act
        linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        // assert: `df = load_users()` resolves via the known return schema
        assert_eq!(linter.dataframes_total, 1);
        assert_eq!(linter.dataframes_typed, 1);
    }

    #[test]
    fn test_should_not_count_unresolved_function_call_assignment() {
        // arrange: `compute()` is not known to return a DataFrame at all, so it must
        // not inflate the coverage denominator
        let source = r#"
def compute():
    return 42

x = compute()
"#;
        let mut linter = Linter::new();

        // act
        linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        // assert
        assert_eq!(linter.dataframes_total, 0);
        assert_eq!(linter.dataframes_typed, 0);
    }

    #[test]
    fn test_should_detect_missing_column_at_direct_call_site() {
        // arrange: postproc requires "c" directly off its own parameter; the caller's
        // loader only provides {a, b}.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("pyproject.toml"), "").unwrap();
        fs::write(
            root.join("loaders.py"),
            r#"
import pandas as pd

def load(path: str) -> pd.DataFrame:
    df = pd.read_csv(path, usecols=["a", "b"])
    return df
"#,
        )
        .unwrap();
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
        let pipeline_source = r#"
from loaders import load
from steps import postproc

def process(path: str) -> None:
    df = load(path)
    result = postproc(df)
    print(result)
"#;
        fs::write(root.join("pipeline.py"), pipeline_source).unwrap();

        // act
        let index = build_index_internal(root);
        let mut linter = Linter::new();
        let pipeline_path = root.join("pipeline.py");
        linter.load_cross_file_symbols(&index, pipeline_source, &pipeline_path, root);
        let errors = linter
            .check_file_internal(pipeline_source, &pipeline_path)
            .unwrap();

        // assert
        assert_eq!(errors.len(), 1, "expected one error, got: {errors:?}");
        assert_eq!(errors[0].code, "missing-column");
        assert!(errors[0].message.contains("missing column(s) {c}"));
        assert!(errors[0].message.contains("available: {a, b}"));
        assert!(errors[0].message.contains("required: {c}"));
        assert!(errors[0].message.contains("passed to postproc"));
    }

    #[test]
    fn test_should_resolve_transitive_requires_through_delegate_chain() {
        // arrange: transform has no direct subscript access of its own — it only
        // delegates df through preproc -> infer -> postproc, which together need
        // {a, b, c}. The loader only supplies {a, b}, so 'c' should surface as a
        // missing-column error at the `transform(df)` call site in pipeline.py,
        // even though 'c' is only referenced two hops down, inside postproc.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("pyproject.toml"), "").unwrap();
        fs::write(
            root.join("loaders.py"),
            r#"
import pandas as pd

def load(path: str) -> pd.DataFrame:
    df = pd.read_csv(path, usecols=["a", "b"])
    return df
"#,
        )
        .unwrap();
        fs::write(
            root.join("steps.py"),
            r#"
import pandas as pd

def preproc(df: pd.DataFrame) -> pd.DataFrame:
    z = df["a"]
    return df

def infer(df: pd.DataFrame) -> pd.DataFrame:
    x = df["b"]
    return df

def postproc(df: pd.DataFrame) -> pd.DataFrame:
    y = df["c"]
    return df

def transform(df: pd.DataFrame) -> pd.DataFrame:
    step1 = preproc(df)
    step2 = infer(step1)
    step3 = postproc(step2)
    return step3
"#,
        )
        .unwrap();
        let pipeline_source = r#"
from loaders import load
from steps import transform

def process(path: str) -> None:
    df = load(path)
    result = transform(df)
    print(result)
"#;
        fs::write(root.join("pipeline.py"), pipeline_source).unwrap();

        // act
        let index = build_index_internal(root);
        let mut linter = Linter::new();
        let pipeline_path = root.join("pipeline.py");
        linter.load_cross_file_symbols(&index, pipeline_source, &pipeline_path, root);
        let errors = linter
            .check_file_internal(pipeline_source, &pipeline_path)
            .unwrap();

        // assert
        assert_eq!(errors.len(), 1, "expected one error, got: {errors:?}");
        assert_eq!(errors[0].code, "missing-column");
        assert!(errors[0].message.contains("missing column(s) {c}"));
        assert!(errors[0].message.contains("required: {a, b, c}"));
        assert!(errors[0].message.contains("passed to transform"));
    }

    #[test]
    fn test_should_not_flag_call_when_all_required_columns_present() {
        // arrange: same delegate chain as above, but the loader supplies {a, b, c} —
        // a strict superset of everything preproc/infer/postproc need.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("pyproject.toml"), "").unwrap();
        fs::write(
            root.join("loaders.py"),
            r#"
import pandas as pd

def load(path: str) -> pd.DataFrame:
    df = pd.read_csv(path, usecols=["a", "b", "c"])
    return df
"#,
        )
        .unwrap();
        fs::write(
            root.join("steps.py"),
            r#"
import pandas as pd

def preproc(df: pd.DataFrame) -> pd.DataFrame:
    z = df["a"]
    return df

def infer(df: pd.DataFrame) -> pd.DataFrame:
    x = df["b"]
    return df

def postproc(df: pd.DataFrame) -> pd.DataFrame:
    y = df["c"]
    return df

def transform(df: pd.DataFrame) -> pd.DataFrame:
    step1 = preproc(df)
    step2 = infer(step1)
    step3 = postproc(step2)
    return step3
"#,
        )
        .unwrap();
        let pipeline_source = r#"
from loaders import load
from steps import transform

def process(path: str) -> None:
    df = load(path)
    result = transform(df)
    print(result)
"#;
        fs::write(root.join("pipeline.py"), pipeline_source).unwrap();

        // act
        let index = build_index_internal(root);
        let mut linter = Linter::new();
        let pipeline_path = root.join("pipeline.py");
        linter.load_cross_file_symbols(&index, pipeline_source, &pipeline_path, root);
        let errors = linter
            .check_file_internal(pipeline_source, &pipeline_path)
            .unwrap();

        // assert
        assert!(errors.is_empty(), "expected no errors, got: {errors:?}");
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
    fn test_should_validate_schema_annotated_parameter_in_body() {
        // arrange: contact_label's parameter is explicitly annotated with
        // CustomerSchema, which does not declare "email" — the bad access should
        // be caught inside the function body itself, standalone, no caller or
        // project index involved.
        let source = r#"
from typedframes import BaseSchema, Column
from typedframes.pandas import PandasFrame

class CustomerSchema(BaseSchema):
    customer_id = Column(type=int)
    name = Column(type=str)

def contact_label(customers: PandasFrame[CustomerSchema]):
    print(customers["name"])
    print(customers["email"])
"#;
        let mut linter = Linter::new();

        // act
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        // assert
        assert_eq!(errors.len(), 1, "expected one error, got: {errors:?}");
        assert!(errors[0].message.contains("email"));
        assert!(errors[0].message.contains("CustomerSchema"));
    }

    #[test]
    fn test_should_use_schema_annotation_as_call_site_requirement() {
        // arrange: contact_label's parameter is annotated with CustomerSchema, which
        // declares {customer_id, name, email} — but the body only ever subscripts
        // "name". The call-site contract must still require all three columns (the
        // annotation is authoritative), not just the one the body happens to touch,
        // so a caller missing "email" is flagged even though nothing in
        // contact_label's body literally writes customers["email"].
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("pyproject.toml"), "").unwrap();
        fs::write(
            root.join("loaders.py"),
            r#"
import pandas as pd

def load(path: str) -> pd.DataFrame:
    df = pd.read_csv(path, usecols=["customer_id", "name"])
    return df
"#,
        )
        .unwrap();
        fs::write(
            root.join("transforms.py"),
            r#"
from typedframes import BaseSchema, Column
from typedframes.pandas import PandasFrame

class CustomerSchema(BaseSchema):
    customer_id = Column(type=int)
    name = Column(type=str)
    email = Column(type=str)

def contact_label(customers: PandasFrame[CustomerSchema]):
    print(customers["name"])
    return customers
"#,
        )
        .unwrap();
        let pipeline_source = r#"
from loaders import load
from transforms import contact_label

def process(path: str) -> None:
    customers = load(path)
    contact_label(customers)
"#;
        fs::write(root.join("pipeline.py"), pipeline_source).unwrap();

        // act
        let index = build_index_internal(root);
        let mut linter = Linter::new();
        let pipeline_path = root.join("pipeline.py");
        linter.load_cross_file_symbols(&index, pipeline_source, &pipeline_path, root);
        let errors = linter
            .check_file_internal(pipeline_source, &pipeline_path)
            .unwrap();

        // assert
        assert_eq!(errors.len(), 1, "expected one error, got: {errors:?}");
        assert_eq!(errors[0].code, "missing-column");
        assert!(errors[0].message.contains("missing column(s) {email}"));
        assert!(errors[0]
            .message
            .contains("required: {customer_id, email, name}"));
    }

    #[test]
    fn test_should_validate_cross_file_schema_annotated_parameter_in_body() {
        // arrange: CustomerSchema lives in its own schemas.py, imported into
        // transforms.py and used as a parameter annotation there — confirms that
        // in-body validation (fix a) works across a file boundary too, not just
        // when the schema is defined in the same file as the function using it.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("pyproject.toml"), "").unwrap();
        fs::write(
            root.join("schemas.py"),
            r#"
from typedframes import BaseSchema, Column

class CustomerSchema(BaseSchema):
    customer_id = Column(type=int)
    name = Column(type=str)
    email = Column(type=str)
"#,
        )
        .unwrap();
        let transforms_source = r#"
from typedframes.pandas import PandasFrame
from schemas import CustomerSchema

def contact_label(customers: PandasFrame[CustomerSchema]):
    print(customers["name"])
    print(customers["not_a_real_column"])
    return customers
"#;
        fs::write(root.join("transforms.py"), transforms_source).unwrap();

        // act: check transforms.py itself, as part of a directory scan (with index)
        let index = build_index_internal(root);
        let mut linter = Linter::new();
        let transforms_path = root.join("transforms.py");
        linter.load_cross_file_symbols(&index, transforms_source, &transforms_path, root);
        let errors = linter
            .check_file_internal(transforms_source, &transforms_path)
            .unwrap();

        // assert
        assert_eq!(errors.len(), 1, "expected one error, got: {errors:?}");
        assert_eq!(errors[0].code, "unknown-column");
        assert!(errors[0].message.contains("not_a_real_column"));
        assert!(errors[0].message.contains("CustomerSchema"));
    }

    #[test]
    fn test_should_validate_variable_whose_schema_is_defined_in_a_third_file() {
        // arrange: schemas.py defines CustomerSchema; loaders.py's load_customers()
        // returns Annotated[pd.DataFrame, CustomerSchema] but does NOT itself define
        // or re-export CustomerSchema; pipeline.py imports only load_customers, never
        // CustomerSchema directly. Before the fix, self.schemas never learned
        // CustomerSchema's columns in pipeline.py's context (load_cross_file_symbols
        // only looked in load_customers' OWN file's entry, which doesn't have it), so
        // ANY access on `customers` silently skipped validation — no false positives,
        // but also no true positives. This must now be caught.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("pyproject.toml"), "").unwrap();
        fs::write(
            root.join("schemas.py"),
            r#"
from typedframes import BaseSchema, Column

class CustomerSchema(BaseSchema):
    customer_id = Column(type=int)
    name = Column(type=str)
"#,
        )
        .unwrap();
        fs::write(
            root.join("loaders.py"),
            r#"
from typing import Annotated
import pandas as pd
from schemas import CustomerSchema

def load_customers(path: str) -> Annotated[pd.DataFrame, CustomerSchema]:
    return pd.read_csv(path, usecols=["customer_id", "name"])
"#,
        )
        .unwrap();
        let pipeline_source = r#"
from loaders import load_customers

def process(path: str) -> None:
    customers = load_customers(path)
    print(customers["not_a_real_column"])
"#;
        fs::write(root.join("pipeline.py"), pipeline_source).unwrap();

        // act
        let index = build_index_internal(root);
        let mut linter = Linter::new();
        let pipeline_path = root.join("pipeline.py");
        linter.load_cross_file_symbols(&index, pipeline_source, &pipeline_path, root);
        let errors = linter
            .check_file_internal(pipeline_source, &pipeline_path)
            .unwrap();

        // assert
        assert_eq!(errors.len(), 1, "expected one error, got: {errors:?}");
        assert_eq!(errors[0].code, "unknown-column");
        assert!(errors[0].message.contains("not_a_real_column"));
        assert!(errors[0].message.contains("CustomerSchema"));
    }

    #[test]
    fn test_should_propagate_case_folded_schema_from_an_unannotated_cross_file_helper() {
        // arrange: an internal-package-style helper in loaders.py queries Snowflake
        // (columns come back upper-cased under sql_dialect="snowflake"), then
        // lower-cases them all before returning -- no BaseSchema/Annotated return type
        // anywhere. pipeline.py, in a different file, only sees the lower-cased names.
        // The cross-file return-schema mechanism must pick up the POST-fold schema
        // (not the raw upper-cased Snowflake one) when resolving load_orders' return
        // type at pipeline.py's call site.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(
            root.join("pyproject.toml"),
            "[tool.typedframes]\nsql_dialect = \"snowflake\"\n",
        )
        .unwrap();
        fs::write(
            root.join("loaders.py"),
            r#"
import pandas as pd
import internal_snowflake_pkg

def load_orders(query: str) -> pd.DataFrame:
    conn = internal_snowflake_pkg.connect()
    cursor = conn.cursor()
    cursor.execute("SELECT order_id, amount FROM orders")
    df = cursor.fetch_pandas_all()
    df.columns = df.columns.str.lower()
    return df
"#,
        )
        .unwrap();
        let pipeline_source = r#"
from loaders import load_orders

def process(query: str) -> None:
    orders = load_orders(query)
    print(orders["order_id"])
    print(orders["ORDER_ID"])
"#;
        fs::write(root.join("pipeline.py"), pipeline_source).unwrap();

        // act
        let index = build_index_internal(root);
        let mut linter = Linter::new();
        linter.with_context(
            root.to_path_buf(),
            &LinterConfig {
                enabled: None,
                warnings: None,
                sql_dialect: Some("snowflake".to_string()),
                trace_external_packages: None,
            },
        );
        let pipeline_path = root.join("pipeline.py");
        linter.load_cross_file_symbols(&index, pipeline_source, &pipeline_path, root);
        let errors = linter
            .check_file_internal(pipeline_source, &pipeline_path)
            .unwrap();

        // assert
        assert_eq!(errors.len(), 1, "expected one error, got: {errors:?}");
        assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
        assert!(errors[0].message.contains("ORDER_ID"));
    }

    #[test]
    fn test_should_trace_an_allowlisted_package_installed_in_venv_site_packages() {
        // arrange: same scenario as the cross-file case-fold test above, but this time
        // the case-folding helper genuinely lives OUTSIDE the project tree, in a fake
        // .venv/lib/pythonX.Y/site-packages/internal_snowflake_pkg/ -- simulating a real
        // pip-installed internal package, not just another first-party file. Without
        // `trace_external_packages` naming it, this must NOT be indexed at all (today's
        // behavior: pipeline.py's call site is untraceable, load_orders' return type is
        // simply unknown); with it, the same post-fold propagation must work across
        // that install boundary.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let site_packages = root
            .join(".venv")
            .join("lib")
            .join("python3.12")
            .join("site-packages");
        let pkg_dir = site_packages.join("internal_snowflake_pkg");
        fs::create_dir_all(&pkg_dir).unwrap();
        fs::write(
            pkg_dir.join("__init__.py"),
            r#"
import pandas as pd

def load_orders(query: str) -> pd.DataFrame:
    conn = connect()
    cursor = conn.cursor()
    cursor.execute("SELECT order_id, amount FROM orders")
    df = cursor.fetch_pandas_all()
    df.columns = df.columns.str.lower()
    return df
"#,
        )
        .unwrap();

        let pipeline_source = r#"
from internal_snowflake_pkg import load_orders

def process(query: str) -> None:
    orders = load_orders(query)
    print(orders["order_id"])
    print(orders["ORDER_ID"])
"#;
        fs::write(root.join("pipeline.py"), pipeline_source).unwrap();

        let config_with_allowlist = LinterConfig {
            enabled: None,
            warnings: None,
            sql_dialect: Some("snowflake".to_string()),
            trace_external_packages: Some(vec!["internal_snowflake_pkg".to_string()]),
        };

        // act: without the allowlist, nothing outside the project tree is indexed.
        fs::write(
            root.join("pyproject.toml"),
            "[tool.typedframes]\nsql_dialect = \"snowflake\"\n",
        )
        .unwrap();
        let index_without_allowlist = build_index_internal(root);
        let mut linter_without = Linter::new();
        linter_without.with_context(root.to_path_buf(), &config_with_allowlist);
        let pipeline_path = root.join("pipeline.py");
        linter_without.load_cross_file_symbols(
            &index_without_allowlist,
            pipeline_source,
            &pipeline_path,
            root,
        );
        let errors_without = linter_without
            .check_file_internal(pipeline_source, &pipeline_path)
            .unwrap();
        assert_eq!(
            errors_without.len(),
            0,
            "load_orders' return type must be untraceable without the allowlist: {errors_without:?}"
        );

        // act: with the allowlist, the external package is indexed and its post-fold
        // return schema is picked up at pipeline.py's call site.
        fs::write(
            root.join("pyproject.toml"),
            "[tool.typedframes]\nsql_dialect = \"snowflake\"\ntrace_external_packages = [\"internal_snowflake_pkg\"]\n",
        )
        .unwrap();
        let index_with_allowlist = build_index_internal(root);
        let mut linter_with = Linter::new();
        linter_with.with_context(root.to_path_buf(), &config_with_allowlist);
        linter_with.load_cross_file_symbols(
            &index_with_allowlist,
            pipeline_source,
            &pipeline_path,
            root,
        );
        let errors_with = linter_with
            .check_file_internal(pipeline_source, &pipeline_path)
            .unwrap();

        // assert
        assert_eq!(
            errors_with.len(),
            1,
            "expected one error, got: {errors_with:?}"
        );
        assert_eq!(errors_with[0].code, CODE_UNKNOWN_COLUMN);
        assert!(errors_with[0].message.contains("ORDER_ID"));
    }

    #[test]
    fn test_should_resolve_param_schema_defined_in_a_third_file_as_call_site_contract() {
        // arrange: schemas.py defines ReportSchema; reports.py's build_report() takes
        // a parameter annotated Annotated[pd.DataFrame, ReportSchema] but does not
        // itself define ReportSchema; pipeline.py calls build_report with a frame
        // that's missing columns ReportSchema declares but build_report's own body
        // never literally subscripts (so the heuristic body-scan fallback alone
        // would under-report the contract). The schema-authoritative override must
        // resolve project-wide, not just when the schema happens to be local.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("pyproject.toml"), "").unwrap();
        fs::write(
            root.join("schemas.py"),
            r#"
from typedframes import BaseSchema, Column

class ReportSchema(BaseSchema):
    a = Column(type=int)
    b = Column(type=int)
    c = Column(type=int)
"#,
        )
        .unwrap();
        fs::write(
            root.join("reports.py"),
            r#"
from typing import Annotated
import pandas as pd
from schemas import ReportSchema

def build_report(df: Annotated[pd.DataFrame, ReportSchema]) -> None:
    print(df["a"])
"#,
        )
        .unwrap();
        let pipeline_source = r#"
import pandas as pd
from reports import build_report

def process(path: str) -> None:
    df = pd.read_csv(path, usecols=["a"])
    build_report(df)
"#;
        fs::write(root.join("pipeline.py"), pipeline_source).unwrap();

        // act
        let index = build_index_internal(root);
        let mut linter = Linter::new();
        let pipeline_path = root.join("pipeline.py");
        linter.load_cross_file_symbols(&index, pipeline_source, &pipeline_path, root);
        let errors = linter
            .check_file_internal(pipeline_source, &pipeline_path)
            .unwrap();

        // assert: 'b' and 'c' are declared on ReportSchema but never subscripted in
        // build_report's own body — only the authoritative schema override catches this.
        assert_eq!(errors.len(), 1, "expected one error, got: {errors:?}");
        assert_eq!(errors[0].code, "missing-column");
        assert!(errors[0].message.contains("missing column(s) {b, c}"));
        assert!(errors[0].message.contains("required: {a, b, c}"));
    }

    #[test]
    fn test_should_track_column_list_slice_as_requirement() {
        // arrange: preproc narrows df to {a, b} via a list-slice. That narrowing
        // itself is a genuine requirement on df's caller (a, b must be present).
        let source = r#"
import pandas as pd

def preproc(df: pd.DataFrame) -> pd.DataFrame:
    slim = df[["a", "b"]]
    return slim
"#;
        let mut linter = Linter::new();
        let _ = linter.check_file_internal(source, Path::new("preproc.py"));

        // assert
        let (required, _) = linter
            .requires
            .get("preproc")
            .expect("preproc should have a recorded requirement");
        assert_eq!(required, &vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn test_should_not_leak_narrowed_variable_misuse_into_caller_requirement() {
        // arrange: `slim = df[["a", "b"]]` gives slim a fully known schema {a, b}
        // (via the existing multi-column-subscript tracking) — a later `slim["c"]`
        // is therefore a certain, local bug in preproc itself, not an ambiguous
        // "maybe missing from the caller" requirement. It must be caught as a
        // direct unknown-column error, and must NOT also inflate preproc's
        // caller-facing contract to {a, b, c} — df's caller only ever needs to
        // supply {a, b}; 'c' is preproc's own mistake in how it uses slim.
        let source = r#"
import pandas as pd

def preproc(df: pd.DataFrame) -> pd.DataFrame:
    slim = df[["a", "b"]]
    z = slim["c"]
    return slim
"#;
        let mut linter = Linter::new();
        let errors = linter
            .check_file_internal(source, Path::new("preproc.py"))
            .unwrap();

        // assert: caught locally as a certain bug ...
        assert_eq!(errors.len(), 1, "expected one error, got: {errors:?}");
        assert_eq!(errors[0].code, "unknown-column");
        assert!(errors[0].message.contains("'c'"));
        assert!(errors[0].message.contains("{a, b}"));

        // ... and NOT folded into the ambiguous caller-facing contract.
        let (required, _) = linter
            .requires
            .get("preproc")
            .expect("preproc should have a recorded requirement");
        assert_eq!(required, &vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn test_should_resolve_transitive_requires_with_list_slice_delegate() {
        // arrange: same delegate chain as
        // test_should_resolve_transitive_requires_through_delegate_chain, but
        // preproc uses a list-slice (`df[["a"]]`) instead of a single-string
        // subscript — this is the exact pattern that originally slipped through
        // the direct-requirement scan.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("pyproject.toml"), "").unwrap();
        fs::write(
            root.join("loaders.py"),
            r#"
import pandas as pd

def load(path: str) -> pd.DataFrame:
    df = pd.read_csv(path, usecols=["a", "b"])
    return df
"#,
        )
        .unwrap();
        fs::write(
            root.join("steps.py"),
            r#"
import pandas as pd

def preproc(df: pd.DataFrame) -> pd.DataFrame:
    return df[["a"]]

def infer(df: pd.DataFrame) -> pd.DataFrame:
    x = df["b"]
    return df

def postproc(df: pd.DataFrame) -> pd.DataFrame:
    y = df["c"]
    return df

def transform(df: pd.DataFrame) -> pd.DataFrame:
    step1 = preproc(df)
    step2 = infer(step1)
    step3 = postproc(step2)
    return step3
"#,
        )
        .unwrap();
        let pipeline_source = r#"
from loaders import load
from steps import transform

def process(path: str) -> None:
    df = load(path)
    result = transform(df)
    print(result)
"#;
        fs::write(root.join("pipeline.py"), pipeline_source).unwrap();

        // act
        let index = build_index_internal(root);
        let mut linter = Linter::new();
        let pipeline_path = root.join("pipeline.py");
        linter.load_cross_file_symbols(&index, pipeline_source, &pipeline_path, root);
        let errors = linter
            .check_file_internal(pipeline_source, &pipeline_path)
            .unwrap();

        // assert: 'a' now correctly contributes via preproc's list-slice, alongside
        // 'b' and 'c' from infer/postproc.
        assert_eq!(errors.len(), 1, "expected one error, got: {errors:?}");
        assert_eq!(errors[0].code, "missing-column");
        assert!(errors[0].message.contains("required: {a, b, c}"));
    }

    #[test]
    fn test_find_project_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let sub = root.join("a/b/c");
        fs::create_dir_all(&sub).unwrap();
        fs::write(root.join("pyproject.toml"), "").unwrap();

        assert_eq!(find_project_root(&sub), root);
        assert_eq!(find_project_root(root), root);
    }

    #[test]
    fn test_is_enabled() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();

        // Case 1: No pyproject.toml -> enabled by default
        assert!(is_enabled(root));

        // Case 2: pyproject.toml without tool section -> enabled by default
        fs::write(
            root.join("pyproject.toml"),
            "[tool.something]\nenabled = false",
        )
        .unwrap();
        assert!(is_enabled(root));

        // Case 3: pyproject.toml with tool.typedframes.enabled = false
        fs::write(
            root.join("pyproject.toml"),
            "[tool.typedframes]\nenabled = false",
        )
        .unwrap();
        assert!(!is_enabled(root));

        // Case 4: pyproject.toml with tool.typedframes.enabled = true
        fs::write(
            root.join("pyproject.toml"),
            "[tool.typedframes]\nenabled = true",
        )
        .unwrap();
        assert!(is_enabled(root));
    }

    #[test]
    fn test_load_linter_config_reads_sql_dialect() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();

        // Absent entirely -> None, so `with_context` leaves the dialect at its
        // Generic default rather than overwriting it.
        fs::write(
            root.join("pyproject.toml"),
            "[tool.typedframes]\nenabled = true",
        )
        .unwrap();
        assert_eq!(load_linter_config(root).sql_dialect, None);

        fs::write(
            root.join("pyproject.toml"),
            "[tool.typedframes]\nsql_dialect = \"snowflake\"",
        )
        .unwrap();
        assert_eq!(
            load_linter_config(root).sql_dialect,
            Some("snowflake".to_string())
        );
    }

    #[test]
    fn test_with_context_sets_dialect_from_config() {
        let mut linter = Linter::new();
        assert_eq!(linter.sql_dialect, sql::SqlDialect::Generic);

        let config = LinterConfig {
            enabled: None,
            warnings: None,
            sql_dialect: Some("snowflake".to_string()),
            trace_external_packages: None,
        };
        linter.with_context(PathBuf::from("/project"), &config);
        assert_eq!(linter.sql_dialect, sql::SqlDialect::Snowflake);
        assert_eq!(linter.project_root, Some(PathBuf::from("/project")));
    }

    #[test]
    fn test_snowflake_dialect_folds_read_sql_columns_to_uppercase_and_flags_lowercase_access() {
        // A Snowflake connection genuinely returns ORDER_ID (uppercase) for an unquoted
        // `order_id` in the SELECT list — see sql::SqlDialect::fold_case. Once
        // sql_dialect is wired to "snowflake", the checker should infer the schema with
        // that real casing, so reading it back with the lowercase spelling actually
        // used in the query text is flagged as unknown-column (a real bug: it would be
        // a KeyError at runtime), while the correctly-cased access passes clean. This
        // is the exact-match-plus-folding behavior chosen over silently accepting both
        // cases.
        let source = r#"
import pandas as pd

df = pd.read_sql("SELECT order_id, amount FROM orders", conn)
print(df["order_id"])
print(df["ORDER_ID"])
"#;
        let mut linter = Linter::new();
        linter.with_context(
            PathBuf::from("/project"),
            &LinterConfig {
                enabled: None,
                warnings: None,
                sql_dialect: Some("snowflake".to_string()),
                trace_external_packages: None,
            },
        );

        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
        assert!(errors[0].message.contains("order_id"));
        assert!(
            errors[0].message.contains("ORDER_ID"),
            "expected a case-corrected suggestion, got: {}",
            errors[0].message
        );
    }

    #[test]
    fn test_generic_dialect_does_not_fold_read_sql_columns() {
        // Without an explicit sql_dialect, columns keep the exact spelling from the
        // query text (SqlDialect::Generic preserves case) — the default, safest
        // behavior for engines the checker hasn't been told about.
        let source = r#"
import pandas as pd

df = pd.read_sql("SELECT order_id, amount FROM orders", conn)
print(df["order_id"])
"#;
        let mut linter = Linter::new();

        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        assert_eq!(errors.len(), 0, "errors: {errors:?}");
    }

    #[test]
    fn test_should_fold_columns_via_rename_with_str_lower() {
        // A Snowflake result genuinely has upper-cased columns; .rename(columns=str.lower)
        // -- a callable, not a literal dict -- should fold the whole known column set to
        // lower case, not be silently treated as a no-op.
        let source = r#"
import pandas as pd

df = pd.read_sql("SELECT order_id, amount FROM orders", conn)
lowered = df.rename(columns=str.lower)
print(lowered["order_id"])
print(lowered["ORDER_ID"])
"#;
        let mut linter = Linter::new();
        linter.with_context(
            PathBuf::from("/project"),
            &LinterConfig {
                enabled: None,
                warnings: None,
                sql_dialect: Some("snowflake".to_string()),
                trace_external_packages: None,
            },
        );

        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
        assert!(errors[0].message.contains("ORDER_ID"));
    }

    #[test]
    fn test_should_fold_columns_via_columns_attribute_assignment() {
        // df.columns = df.columns.str.lower() -- an attribute-assignment target, not a
        // method-chain call -- should be recognized the same way as the rename() form.
        let source = r#"
import pandas as pd

df = pd.read_sql("SELECT order_id, amount FROM orders", conn)
df.columns = df.columns.str.lower()
print(df["order_id"])
print(df["ORDER_ID"])
"#;
        let mut linter = Linter::new();
        linter.with_context(
            PathBuf::from("/project"),
            &LinterConfig {
                enabled: None,
                warnings: None,
                sql_dialect: Some("snowflake".to_string()),
                trace_external_packages: None,
            },
        );

        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
        assert!(errors[0].message.contains("ORDER_ID"));
    }

    #[test]
    fn test_should_not_fold_columns_for_unrecognized_transform_function() {
        // A custom/arbitrary transform function is NOT reverse-engineered -- the base
        // schema passes through unchanged (neither folded nor flagged), same as any
        // other unrecognized rename() argument shape.
        let source = r#"
import pandas as pd
import my_internal_pkg

df = pd.read_sql("SELECT order_id, amount FROM orders", conn)
lowered = df.rename(columns=my_internal_pkg.normalize)
print(lowered["ORDER_ID"])
"#;
        let mut linter = Linter::new();
        linter.with_context(
            PathBuf::from("/project"),
            &LinterConfig {
                enabled: None,
                warnings: None,
                sql_dialect: Some("snowflake".to_string()),
                trace_external_packages: None,
            },
        );

        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        // The base schema passes through UNCHANGED (still upper-cased) -- not folded,
        // but also not flagged as an error just for being an unrecognized call shape.
        assert_eq!(
            errors.len(),
            0,
            "unrecognized transform should pass the pre-fold base schema through untouched: {errors:?}"
        );
    }

    #[test]
    fn test_traces_sql_through_a_single_assigned_variable() {
        // A module-level (or function-local — the checker doesn't distinguish, see
        // StringBindingCollector) constant assigned exactly once should resolve just
        // like an inline literal.
        let source = r#"
import pandas as pd

QUERY = "SELECT order_id, amount FROM orders"
df = pd.read_sql(QUERY, conn)
print(df["order_id"])
print(df["missing"])
"#;
        let mut linter = Linter::new();
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert!(errors[0].message.contains("missing"));
    }

    #[test]
    fn test_reassigned_sql_variable_is_not_traced() {
        // A second binding of QUERY poisons it — the checker must not guess which
        // assignment was actually in effect at the read_sql call, so the load falls
        // through to the untracked-dataframe hint instead of inferring a (possibly
        // wrong) column set.
        let source = r#"
import pandas as pd

QUERY = "SELECT order_id FROM orders"
QUERY = "SELECT customer_id FROM customers"
df = pd.read_sql(QUERY, conn)
"#;
        let mut linter = Linter::new();
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert_eq!(errors[0].code, CODE_UNTRACKED_DATAFRAME);
    }

    #[test]
    fn test_traces_sql_from_a_file_under_the_project_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::write(
            root.join("orders.sql"),
            "SELECT order_id, amount FROM orders",
        )
        .unwrap();

        let source = r#"
import pandas as pd
from pathlib import Path

sql = Path("orders.sql").read_text()
df = pd.read_sql(sql, conn)
print(df["order_id"])
print(df["missing"])
"#;
        let mut linter = Linter::new();
        linter.with_context(root.to_path_buf(), &LinterConfig::EMPTY);
        let errors = linter
            .check_file_internal(source, &root.join("pipeline.py"))
            .unwrap();

        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert!(errors[0].message.contains("missing"));
    }

    #[test]
    fn test_file_read_refuses_path_escaping_project_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let outside = temp.path().parent().unwrap();
        fs::write(outside.join("secret.sql"), "SELECT * FROM secrets").unwrap();

        let source = r#"
import pandas as pd
from pathlib import Path

sql = Path("../secret.sql").read_text()
df = pd.read_sql(sql, conn)
"#;
        let mut linter = Linter::new();
        linter.with_context(root.to_path_buf(), &LinterConfig::EMPTY);
        let errors = linter
            .check_file_internal(source, &root.join("pipeline.py"))
            .unwrap();

        // Unresolved -> falls through to untracked-dataframe, never a fabricated
        // column set from a file outside the project.
        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert_eq!(errors[0].code, CODE_UNTRACKED_DATAFRAME);
    }

    #[test]
    fn test_file_read_refuses_non_sql_extension() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::write(root.join("orders.txt"), "SELECT order_id FROM orders").unwrap();

        let source = r#"
import pandas as pd
from pathlib import Path

sql = Path("orders.txt").read_text()
df = pd.read_sql(sql, conn)
"#;
        let mut linter = Linter::new();
        linter.with_context(root.to_path_buf(), &LinterConfig::EMPTY);
        let errors = linter
            .check_file_internal(source, &root.join("pipeline.py"))
            .unwrap();

        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert_eq!(errors[0].code, CODE_UNTRACKED_DATAFRAME);
    }

    #[test]
    fn test_traces_sql_from_file_via_path_file_parent() {
        // Path(__file__).parent / "orders.sql" resolves to an ABSOLUTE path (since the
        // file being checked is passed in as an absolute path by every real caller) —
        // regression test for a bug where read_sql_file unconditionally refused
        // absolute paths and could never actually accept this legitimate idiom.
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::write(
            root.join("orders.sql"),
            "SELECT order_id, amount FROM orders",
        )
        .unwrap();

        let source = r#"
import pandas as pd
from pathlib import Path

sql = (Path(__file__).parent / "orders.sql").read_text()
df = pd.read_sql(sql, conn)
print(df["order_id"])
print(df["missing"])
"#;
        let mut linter = Linter::new();
        linter.with_context(root.to_path_buf(), &LinterConfig::EMPTY);
        let errors = linter
            .check_file_internal(source, &root.join("pipeline.py"))
            .unwrap();

        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert!(errors[0].message.contains("missing"));
    }

    #[test]
    fn test_file_read_refuses_absolute_path_outside_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let outside = temp.path().parent().unwrap();
        let secret_path = outside.join("secret.sql");
        fs::write(&secret_path, "SELECT * FROM secrets").unwrap();

        let source = format!(
            r#"
import pandas as pd
from pathlib import Path

sql = Path("{}").read_text()
df = pd.read_sql(sql, conn)
"#,
            secret_path.display()
        );
        let mut linter = Linter::new();
        linter.with_context(root.to_path_buf(), &LinterConfig::EMPTY);
        let errors = linter
            .check_file_internal(&source, &root.join("pipeline.py"))
            .unwrap();

        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert_eq!(errors[0].code, CODE_UNTRACKED_DATAFRAME);
    }

    #[test]
    fn test_fstring_sql_is_silently_untracked_not_a_new_diagnostic() {
        // f-string SQL is refused (it's the injection anti-pattern parameterized
        // queries exist to avoid), but the checker doesn't have taint analysis to tell
        // a safe interpolation from a real vulnerability, so it stays silent rather
        // than emitting an unactionable injection warning — just the same
        // untracked-dataframe hint as any other unresolvable load.
        let source = r#"
import pandas as pd

cols = "order_id, amount"
df = pd.read_sql(f"SELECT {cols} FROM orders", conn)
"#;
        let mut linter = Linter::new();
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert_eq!(errors[0].code, CODE_UNTRACKED_DATAFRAME);
    }

    #[test]
    fn test_declarative_model_20_style_registers_mapped_columns() {
        let source = r#"
from sqlalchemy.orm import Mapped, mapped_column, DeclarativeBase

class Base(DeclarativeBase):
    pass

class Order(Base):
    __tablename__ = "orders"
    id: Mapped[int] = mapped_column(primary_key=True)
    amount: Mapped[float]
    customer_items: Mapped[list["Item"]]
"#;
        let mut linter = Linter::new();
        linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        let cols = linter
            .schemas
            .get("Order")
            .expect("Order should be registered");
        assert!(cols.contains(&"id".to_string()), "cols: {cols:?}");
        assert!(cols.contains(&"amount".to_string()), "cols: {cols:?}");
        assert!(
            !cols.contains(&"customer_items".to_string()),
            "relationship attribute should be excluded: {cols:?}"
        );
        assert!(
            !cols.contains(&"__tablename__".to_string()),
            "dunder should be excluded: {cols:?}"
        );
    }

    #[test]
    fn test_declarative_model_legacy_column_style_and_positional_db_name() {
        let source = r#"
from sqlalchemy import Column, Integer, String
from sqlalchemy.orm import declarative_base, relationship

Base = declarative_base()

class Order(Base):
    __tablename__ = "orders"
    id = Column("order_id", Integer, primary_key=True)
    amount = Column(Integer)
    customer = relationship("Customer")
"#;
        let mut linter = Linter::new();
        linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        let cols = linter
            .schemas
            .get("Order")
            .expect("Order should be registered");
        // Both the attribute name and the positional DB-name override are registered.
        assert!(cols.contains(&"id".to_string()), "cols: {cols:?}");
        assert!(cols.contains(&"order_id".to_string()), "cols: {cols:?}");
        assert!(cols.contains(&"amount".to_string()), "cols: {cols:?}");
        assert!(
            !cols.contains(&"customer".to_string()),
            "relationship() should be excluded: {cols:?}"
        );
    }

    #[test]
    fn test_orm_model_skips_reserved_method_name_check() {
        // "count" would trip CODE_RESERVED_NAME on a typedframes-native BaseSchema, but
        // an ORM model's column names come from an external database the user doesn't
        // control renaming — the check must not fire here.
        let source = r#"
from sqlalchemy.orm import Mapped, mapped_column

class Order(Base):
    __tablename__ = "orders"
    id: Mapped[int] = mapped_column(primary_key=True)
    count: Mapped[int]
"#;
        let mut linter = Linter::new();
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        assert!(
            errors.iter().all(|e| e.code != CODE_RESERVED_NAME),
            "errors: {errors:?}"
        );
    }

    #[test]
    fn test_select_columns_resolve_inline_in_read_sql() {
        let source = r#"
import pandas as pd
from sqlalchemy import select
from sqlalchemy.orm import Mapped, mapped_column

class Order(Base):
    __tablename__ = "orders"
    id: Mapped[int] = mapped_column(primary_key=True)
    amount: Mapped[float]

df = pd.read_sql(select(Order.id, Order.amount), engine)
print(df["id"])
print(df["missing"])
"#;
        let mut linter = Linter::new();
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert!(errors[0].message.contains("missing"));
    }

    #[test]
    fn test_select_columns_resolve_through_a_bound_variable_and_label() {
        let source = r#"
import pandas as pd
from sqlalchemy import select
from sqlalchemy.orm import Mapped, mapped_column

class Order(Base):
    __tablename__ = "orders"
    id: Mapped[int] = mapped_column(primary_key=True)
    amount: Mapped[float]

stmt = select(Order.id, Order.amount.label("total")).where(Order.amount > 0)
df = pd.read_sql(stmt, engine)
print(df["total"])
print(df["missing"])
"#;
        let mut linter = Linter::new();
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert!(errors[0].message.contains("missing"));
    }

    #[test]
    fn test_bare_select_of_model_is_unresolved() {
        // select(Order) — pulling "all" of a model's columns — is deliberately not
        // supported: the ORM extractor is allowlist-based and can under-extract on an
        // unusual declarative pattern, so treating its output as "the complete column
        // set" here would risk a false unknown-column later.
        let source = r#"
import pandas as pd
from sqlalchemy import select
from sqlalchemy.orm import Mapped, mapped_column

class Order(Base):
    __tablename__ = "orders"
    id: Mapped[int] = mapped_column(primary_key=True)

df = pd.read_sql(select(Order), engine)
"#;
        let mut linter = Linter::new();
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert_eq!(errors[0].code, CODE_UNTRACKED_DATAFRAME);
    }

    #[test]
    fn test_feast_chained_form_registers_open_schema_over_feature_columns() {
        // The critical case: df["driver_id"] is an entity join key, NOT one of the
        // features= names, and is the first line of the canonical Feast tutorial. If
        // this were an exact-match schema it would be a false unknown-column — the
        // whole reason register_feast_dataframe marks it open instead.
        let source = r#"
df = store.get_historical_features(
    entity_df=entity_df,
    features=["driver_stats:conv_rate", "driver_stats:acc_rate"],
).to_df()
print(df["conv_rate"])
print(df["driver_id"])
"#;
        let mut linter = Linter::new();
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        assert_eq!(errors.len(), 0, "errors: {errors:?}");
        let schema_name = &linter.variables.get("df").unwrap().0;
        assert!(linter.open_schemas.contains(schema_name));
    }

    #[test]
    fn test_feast_split_form_job_not_treated_as_dataframe_before_to_df() {
        let source = r#"
job = store.get_historical_features(
    entity_df=entity_df,
    features=["driver_stats:conv_rate"],
)
df = job.to_df()
print(df["conv_rate"])
print(df["driver_id"])
"#;
        let mut linter = Linter::new();
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        assert_eq!(errors.len(), 0, "errors: {errors:?}");
        assert!(
            !linter.variables.contains_key("job"),
            "job (a RetrievalJob, not a DataFrame) should not be tracked in self.variables"
        );
    }

    #[test]
    fn test_feast_full_feature_names_uses_double_underscore() {
        let source = r#"
df = store.get_historical_features(
    entity_df=entity_df,
    features=["driver_stats:conv_rate"],
    full_feature_names=True,
).to_df()
print(df["driver_stats__conv_rate"])
"#;
        let mut linter = Linter::new();
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        assert_eq!(errors.len(), 0, "errors: {errors:?}");
    }

    #[test]
    fn test_feast_unresolvable_features_falls_through_to_untracked_dataframe() {
        let source = r#"
feature_names = get_feature_list()
df = store.get_historical_features(
    entity_df=entity_df,
    features=feature_names,
).to_df()
"#;
        let mut linter = Linter::new();
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert_eq!(errors[0].code, CODE_UNTRACKED_DATAFRAME);
    }

    #[test]
    fn test_unrelated_to_df_call_is_left_alone() {
        // `.to_df()` on something that never went through a recognized Feast
        // retrieval call must not be treated as a Feast result at all.
        let source = r#"
df = some_other_object.to_df()
"#;
        let mut linter = Linter::new();
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        assert_eq!(errors.len(), 0, "errors: {errors:?}");
        assert!(!linter.variables.contains_key("df"));
    }

    #[test]
    fn test_bigquery_chained_query_to_dataframe_resolves_columns() {
        let source = r#"
df = client.query("SELECT user_id, total_spent FROM analytics.customers").to_dataframe()
print(df["user_id"])
print(df["missing"])
"#;
        let mut linter = Linter::new();
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert!(errors[0].message.contains("missing"));
    }

    #[test]
    fn test_pyspark_chained_sql_to_pandas_resolves_columns() {
        let source = r#"
df = spark.sql("SELECT order_id, amount FROM orders").toPandas()
print(df["order_id"])
print(df["missing"])
"#;
        let mut linter = Linter::new();
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert!(errors[0].message.contains("missing"));
    }

    #[test]
    fn test_duckdb_chained_sql_df_resolves_columns() {
        let source = r#"
df = duckdb.sql("SELECT order_id, amount FROM 'orders.parquet'").df()
print(df["order_id"])
print(df["missing"])
"#;
        let mut linter = Linter::new();
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert!(errors[0].message.contains("missing"));
    }

    #[test]
    fn test_unrelated_df_method_is_left_alone_not_treated_as_sql() {
        // `.df()` chained onto something that ISN'T one of SQL_PRODUCING_METHODS must
        // not be treated as a SQL-producing call at all — no dataframes_total bump, no
        // untracked-dataframe warning, since plenty of unrelated code has a `.df()`
        // accessor for something else entirely.
        let source = r#"
df = some_relation.transform().df()
"#;
        let mut linter = Linter::new();
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        assert_eq!(errors.len(), 0, "errors: {errors:?}");
        assert_eq!(linter.dataframes_total, 0);
    }

    #[test]
    fn test_snowflake_cursor_execute_then_fetch_pandas_all_resolves_columns() {
        let source = r#"
cursor.execute("SELECT order_id, amount FROM orders")
df = cursor.fetch_pandas_all()
print(df["order_id"])
print(df["missing"])
"#;
        let mut linter = Linter::new();
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert!(errors[0].message.contains("missing"));
    }

    #[test]
    fn test_cursor_reexecute_with_unresolvable_query_poisons_previous_entry() {
        // A second execute() with an unresolvable (variable) query must not leave the
        // first execute()'s columns silently in effect for the next fetch.
        let source = r#"
cursor.execute("SELECT order_id FROM orders")
cursor.execute(dynamic_query)
df = cursor.fetch_pandas_all()
"#;
        let mut linter = Linter::new();
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert_eq!(errors[0].code, CODE_UNTRACKED_DATAFRAME);
    }

    #[test]
    fn test_connectorx_read_sql_uses_second_positional_argument() {
        let source = r#"
import connectorx as cx

df = cx.read_sql(conn_uri, "SELECT order_id, amount FROM orders")
print(df["order_id"])
print(df["missing"])
"#;
        let mut linter = Linter::new();
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert!(errors[0].message.contains("missing"));
    }

    #[test]
    fn test_levenshtein() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("flaw", "lawn"), 2);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("equal", "equal"), 0);
    }

    #[test]
    fn test_extract_schema_from_annotation() {
        let source = "x: PandasFrame[MySchema] = df";
        let parsed = parse_module(source).unwrap();
        let stmt = &parsed.into_syntax().body[0];
        if let Stmt::AnnAssign(ann) = stmt {
            let schema = Linter::extract_schema_from_annotation(&ann.annotation);
            assert_eq!(schema, Some("MySchema"));
        } else {
            panic!("Expected AnnAssign");
        }
    }

    #[test]
    fn test_visitor_various_stmts() {
        let source = r#"
class Other: pass
def func(): pass
x = 1
"#;
        let mut linter = Linter::new();
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();
        assert!(errors.is_empty());
    }

    #[test]
    fn test_no_false_positive_on_method_call_name() {
        // `df.assign(created_at="2024-01-01")` must NOT raise
        // "Column 'assign' does not exist" — method names are not column accesses.
        let source = r#"
from typedframes import BaseSchema, Column
from typedframes.pandas import PandasFrame

class UserData(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)

import pandas as pd

df: PandasFrame[UserData] = pd.read_csv("users.csv")
augmented = df.assign(created_at="2024-01-01")
print(augmented["user_id"])
"#;
        let mut linter = Linter::new();
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    }

    #[test]
    fn test_should_validate_pl_col_in_select() {
        // arrange
        let source = r#"
from typedframes import BaseSchema, Column
from typedframes.polars import PolarsFrame
import polars as pl

class OrderSchema(BaseSchema):
    order_id = Column(type=int)
    amount = Column(type=float)

df: PolarsFrame[OrderSchema] = pl.read_csv("orders.csv")
result = df.select(pl.col("amount"))
bad = df.select(pl.col("revenue"))
"#;
        let mut linter = Linter::new();

        // act
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        // assert
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("revenue"));
        assert!(errors[0].message.contains("OrderSchema"));
    }

    #[test]
    fn test_should_validate_pl_col_in_filter() {
        // arrange
        let source = r#"
from typedframes import BaseSchema, Column
from typedframes.polars import PolarsFrame
import polars as pl

class UserSchema(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)

df: PolarsFrame[UserSchema] = pl.read_csv("users.csv")
result = df.filter(pl.col("user_id") > 10)
bad = df.filter(pl.col("username") == "alice")
"#;
        let mut linter = Linter::new();

        // act
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        // assert
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("username"));
    }

    #[test]
    fn test_should_validate_pl_col_list_in_select() {
        // arrange
        let source = r#"
from typedframes import BaseSchema, Column
from typedframes.polars import PolarsFrame
import polars as pl

class SalesSchema(BaseSchema):
    region = Column(type=str)
    revenue = Column(type=float)

df: PolarsFrame[SalesSchema] = pl.read_csv("sales.csv")
result = df.select([pl.col("region"), pl.col("revenue")])
bad = df.select([pl.col("region"), pl.col("profit")])
"#;
        let mut linter = Linter::new();

        // act
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        // assert
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("profit"));
    }

    #[test]
    fn test_should_validate_bare_col_import() {
        // arrange
        let source = r#"
from typedframes import BaseSchema, Column
from typedframes.polars import PolarsFrame
from polars import col

class ItemSchema(BaseSchema):
    item_id = Column(type=int)
    price = Column(type=float)

df: PolarsFrame[ItemSchema] = None
result = df.select(col("price"))
bad = df.select(col("cost"))
"#;
        let mut linter = Linter::new();

        // act
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        // assert
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("cost"));
    }

    #[test]
    fn test_should_validate_chained_pl_col() {
        // arrange
        let source = r#"
from typedframes import BaseSchema, Column
from typedframes.polars import PolarsFrame
import polars as pl

class StockSchema(BaseSchema):
    ticker = Column(type=str)
    close = Column(type=float)

df: PolarsFrame[StockSchema] = pl.read_csv("stocks.csv")
result = df.filter(pl.col("close").is_not_null())
bad = df.filter(pl.col("open").is_not_null())
"#;
        let mut linter = Linter::new();

        // act
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        // assert
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("open"));
    }

    #[test]
    fn test_should_pass_valid_pl_col() {
        // arrange
        let source = r#"
from typedframes import BaseSchema, Column
from typedframes.polars import PolarsFrame
import polars as pl

class MetricsSchema(BaseSchema):
    date = Column(type=str)
    value = Column(type=float)

df: PolarsFrame[MetricsSchema] = pl.read_csv("metrics.csv")
filtered = df.filter(pl.col("value") > 100)
selected = df.select([pl.col("date"), pl.col("value")])
"#;
        let mut linter = Linter::new();

        // act
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        // assert
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    }

    #[test]
    fn test_should_skip_pl_col_on_untracked_variable() {
        // arrange — variable has no schema (returned from opaque function), so no validation should occur
        let source = r#"
import polars as pl

df = some_function()
result = df.filter(pl.col("nonexistent_column") > 0)
"#;
        let mut linter = Linter::new();

        // act
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        // assert — untracked variable, no column validation
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    }

    #[test]
    fn test_should_ignore_all_on_bare_ignore_comment() {
        // arrange — bare `# typedframes: ignore` suppresses all diagnostics on that line
        let source = r#"
from typedframes import BaseSchema, Column

class S(BaseSchema):
    user_id = Column(type=int)

import pandas as pd
df = pd.read_csv("data.csv", usecols=["user_id"])
print(df["revenue"])  # typedframes: ignore
"#;
        let mut linter = Linter::new();

        // act
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        // assert — error on that line is suppressed
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    }

    #[test]
    fn test_should_ignore_specific_code() {
        // arrange — `# typedframes: ignore[unknown-column]` suppresses only that code
        let source = r#"
from typedframes import BaseSchema, Column

class S(BaseSchema):
    user_id = Column(type=int)

import pandas as pd
df = pd.read_csv("data.csv", usecols=["user_id"])
print(df["revenue"])  # typedframes: ignore[unknown-column]
"#;
        let mut linter = Linter::new();

        // act
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        // assert — unknown-column on that line is suppressed
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    }

    #[test]
    fn test_should_not_ignore_mismatched_code() {
        // arrange — `# typedframes: ignore[dropped-unknown-column]` does not suppress unknown-column
        let source = r#"
from typedframes import BaseSchema, Column

class S(BaseSchema):
    user_id = Column(type=int)

import pandas as pd
df = pd.read_csv("data.csv", usecols=["user_id"])
print(df["revenue"])  # typedframes: ignore[dropped-unknown-column]
"#;
        let mut linter = Linter::new();

        // act
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        // assert — wrong code in brackets, so error is NOT suppressed
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
    }

    #[test]
    fn test_should_ignore_comma_separated_codes() {
        // arrange — `# typedframes: ignore[unknown-column, dropped-unknown-column]`
        let source = r#"
from typedframes import BaseSchema, Column

class S(BaseSchema):
    user_id = Column(type=int)

import pandas as pd
df = pd.read_csv("data.csv", usecols=["user_id"])
print(df["revenue"])  # typedframes: ignore[unknown-column, dropped-unknown-column]
"#;
        let mut linter = Linter::new();

        // act
        let errors = linter
            .check_file_internal(source, Path::new("test.py"))
            .unwrap();

        // assert — unknown-column is in the comma-separated list, so suppressed
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    }

    #[test]
    fn test_should_detect_missing_column_through_plain_module_import_attribute_call() {
        // arrange: same shape as test_should_detect_missing_column_at_direct_call_site,
        // but pipeline.py calls `steps.postproc(df)` via a plain `import steps` rather
        // than `from steps import postproc` — the blind spot this test guards against.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("pyproject.toml"), "").unwrap();
        fs::write(
            root.join("loaders.py"),
            r#"
import pandas as pd

def load(path: str) -> pd.DataFrame:
    df = pd.read_csv(path, usecols=["a", "b"])
    return df
"#,
        )
        .unwrap();
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
        let pipeline_source = r#"
import steps
from loaders import load

def process(path: str) -> None:
    df = load(path)
    result = steps.postproc(df)
    print(result)
"#;
        fs::write(root.join("pipeline.py"), pipeline_source).unwrap();

        // act
        let index = build_index_internal(root);
        let mut linter = Linter::new();
        let pipeline_path = root.join("pipeline.py");
        linter.load_cross_file_symbols(&index, pipeline_source, &pipeline_path, root);
        let errors = linter
            .check_file_internal(pipeline_source, &pipeline_path)
            .unwrap();

        // assert
        assert_eq!(errors.len(), 1, "expected one error, got: {errors:?}");
        assert_eq!(errors[0].code, "missing-column");
        assert!(errors[0].message.contains("missing column(s) {c}"));
        assert!(errors[0].message.contains("passed to postproc"));
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

    #[test]
    fn test_should_detect_missing_column_through_wildcard_import() {
        // arrange: pipeline.py brings postproc into scope via `from steps import *`
        // rather than naming it explicitly.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("pyproject.toml"), "").unwrap();
        fs::write(
            root.join("loaders.py"),
            r#"
import pandas as pd

def load(path: str) -> pd.DataFrame:
    df = pd.read_csv(path, usecols=["a", "b"])
    return df
"#,
        )
        .unwrap();
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
        let pipeline_source = r#"
from steps import *
from loaders import load

def process(path: str) -> None:
    df = load(path)
    result = postproc(df)
    print(result)
"#;
        fs::write(root.join("pipeline.py"), pipeline_source).unwrap();

        // act
        let index = build_index_internal(root);
        let mut linter = Linter::new();
        let pipeline_path = root.join("pipeline.py");
        linter.load_cross_file_symbols(&index, pipeline_source, &pipeline_path, root);
        let errors = linter
            .check_file_internal(pipeline_source, &pipeline_path)
            .unwrap();

        // assert
        assert_eq!(errors.len(), 1, "expected one error, got: {errors:?}");
        assert_eq!(errors[0].code, "missing-column");
        assert!(errors[0].message.contains("missing column(s) {c}"));
        assert!(errors[0].message.contains("passed to postproc"));
    }

    // ── Benchmark: site-packages resolution overhead ────────────────────────────
    //
    // `cargo bench` (criterion) can't link in this environment -- pyo3's
    // "extension-module" feature deliberately omits libpython, which is fine for the
    // cdylib actually loaded by Python but breaks any standalone executable built from
    // this crate (the criterion harness's own `main()`, same as the `typedframes_checker`
    // `[[bin]]` target). `cargo test --lib` reliably links (nothing here calls into the
    // pyo3-macro-generated FFI glue), so these are ordinary #[test]s used as a timing
    // vehicle, not correctness assertions -- run explicitly with:
    //   cargo test --lib bench_ -- --ignored --nocapture

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

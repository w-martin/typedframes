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

use pyo3::prelude::*;
use ruff_python_ast::{self as ast, Expr, Stmt};
use ruff_python_parser::parse_module;
use ruff_source_file::{LineIndex, SourceCode};
use ruff_text_size::Ranged;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
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
        return Ok("[]".to_string());
    }

    let source = fs::read_to_string(path)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyIOError, _>(format!("{}", e)))?;

    let mut linter = Linter::new();

    if let Some(bytes) = index_bytes {
        if let Ok(index) = rmp_serde::from_slice::<ProjectIndex>(&bytes) {
            linter.load_cross_file_symbols(&index, &source, path, &project_root);
        }
    }

    let mut errors = linter
        .check_file_internal(&source, path)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("{}", e)))?;

    if !config.warnings.unwrap_or(true) {
        errors.retain(|e| e.severity != "warning");
    }

    serde_json::to_string(&errors)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("{}", e)))
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
// All fields are optional; absent keys default to `true`.
#[derive(serde::Deserialize)]
struct LinterConfig {
    enabled: Option<bool>,  // default: true
    warnings: Option<bool>, // default: true
}

// Read `[tool.typedframes]` from `pyproject.toml` at `project_root`.
// Returns a config with all fields `None` if the file is absent, unreadable, or has no
// `[tool.typedframes]` section; callers use `.unwrap_or(true)` on each field.
fn load_linter_config(project_root: &Path) -> LinterConfig {
    let config_path = project_root.join("pyproject.toml");
    if !config_path.exists() {
        return LinterConfig {
            enabled: None,
            warnings: None,
        };
    }

    let content = match fs::read_to_string(config_path) {
        Ok(c) => c,
        Err(_) => {
            return LinterConfig {
                enabled: None,
                warnings: None,
            }
        }
    };

    let config: Config = match toml::from_str(&content) {
        Ok(c) => c,
        Err(_) => {
            return LinterConfig {
                enabled: None,
                warnings: None,
            }
        }
    };

    config
        .tool
        .and_then(|t| t.typedframes)
        .unwrap_or(LinterConfig {
            enabled: None,
            warnings: None,
        })
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

// Return-type information extracted from an annotated function definition.
#[derive(Serialize, Deserialize)]
struct IndexFunction {
    returns_schema: String, // BaseSchema subclass name, e.g. "OrderSchema"; empty if none
    returns_frame: String,  // reserved for future use
}

// Symbol table for a single `.py` file, stored inside ProjectIndex.
#[derive(Serialize, Deserialize)]
struct IndexEntry {
    schemas: HashMap<String, Vec<String>>, // schema name -> column list
    functions: HashMap<String, IndexFunction>, // function name -> return type info
    exports: Vec<String>,                  // names in __all__, for wildcard-import resolution
}

// In-memory cross-file symbol index.
// Serialised as MessagePack so it can be held in Python memory and passed to check_file
// without any intermediate disk I/O.  The version field allows future format migrations.
#[derive(Serialize, Deserialize)]
struct ProjectIndex {
    version: u32,                       // format version, currently always 1
    files: HashMap<String, IndexEntry>, // absolute file path -> IndexEntry
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

// Parse one `.py` file and extract its symbols into an IndexEntry.
// Runs the linter in index mode (diagnostics discarded) to collect schemas and
// functions, then separately parses `__all__` assignments for wildcard-import support.
fn index_file(path: &Path) -> Option<IndexEntry> {
    let source = fs::read_to_string(path).ok()?;

    let mut linter = Linter::new();
    let _ = linter.check_file_internal(&source, path);

    let schemas = linter.schemas;
    let functions: HashMap<String, IndexFunction> = linter
        .functions
        .into_iter()
        .map(|(k, v)| {
            (
                k,
                IndexFunction {
                    returns_schema: v,
                    returns_frame: String::new(),
                },
            )
        })
        .collect();

    let exports = parse_module(&source)
        .ok()
        .map(|parsed| {
            let module = parsed.into_syntax();
            let mut names = Vec::new();
            for stmt in &module.body {
                let Stmt::Assign(assign) = stmt else {
                    continue;
                };
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
                            names.push(s.value.to_str().to_string());
                        }
                    }
                }
            }
            names
        })
        .unwrap_or_default();

    Some(IndexEntry {
        schemas,
        functions,
        exports,
    })
}

// Build a ProjectIndex by indexing every `.py` file under `project_root`.
fn build_index_internal(project_root: &Path) -> ProjectIndex {
    let py_files = collect_py_files(project_root);
    let mut files = HashMap::new();
    for file_path in py_files {
        if let Some(entry) = index_file(&file_path) {
            if let Some(path_str) = file_path.to_str() {
                files.insert(path_str.to_string(), entry);
            }
        }
    }
    ProjectIndex { version: 1, files }
}

// ──────────────────────────────────────────────────────────────────────────────
// Diagnostic codes
// ──────────────────────────────────────────────────────────────────────────────

const CODE_UNKNOWN_COLUMN: &str = "unknown-column";
const CODE_RESERVED_NAME: &str = "reserved-name";
const CODE_UNTRACKED_DATAFRAME: &str = "untracked-dataframe";
const CODE_DROPPED_UNKNOWN_COLUMN: &str = "dropped-unknown-column";

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
];

const LOAD_MODULES: &[&str] = &["pd", "pandas", "pl", "polars"];

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
    line_index: Option<LineIndex>,
    source: String,
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
            line_index: None,
            source: String::new(),
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

    // Parse `source`, walk the AST, then filter out any diagnostic whose line carries a
    // `# typedframes: ignore` comment.  Returns the surviving errors.
    pub fn check_file_internal(
        &mut self,
        source: &str,
        _path: &Path,
    ) -> Result<Vec<LintError>, anyhow::Error> {
        self.source = source.to_string();
        self.line_index = Some(LineIndex::from_source_text(source));
        let parsed = parse_module(source).map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut errors = Vec::new();

        for stmt in parsed.into_syntax().body {
            self.visit_stmt(&stmt, &mut errors);
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
            let mod_path = module_name.replace('.', "/");
            let candidates = [
                project_root.join(format!("{mod_path}.py")),
                project_root.join("src").join(format!("{mod_path}.py")),
            ];
            let Some(resolved_path) = candidates.iter().find(|p| p.exists()) else {
                continue;
            };
            let Some(resolved_str) = resolved_path.to_str() else {
                continue;
            };
            let Some(entry) = index.files.get(resolved_str) else {
                continue;
            };
            for alias in &import_from.names {
                let name = alias.name.id.as_str();
                if let Some(cols) = entry.schemas.get(name) {
                    self.schemas.insert(name.to_string(), cols.clone());
                }
                if let Some(func) = entry.functions.get(name) {
                    self.functions
                        .insert(name.to_string(), func.returns_schema.clone());
                    if let Some(schema_cols) = entry.schemas.get(&func.returns_schema) {
                        self.schemas
                            .insert(func.returns_schema.clone(), schema_cols.clone());
                    }
                }
            }
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

    // Check if a type name is a DataFrame/Frame type
    fn is_frame_type(name: &str) -> bool {
        matches!(name, "DataFrame" | "PandasFrame" | "PolarsFrame")
    }

    // Extract schema name from a type annotation like PandasFrame[Schema]
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

    // Extract column names from a load function call (usecols/columns kwarg or dtype/schema dict keys).
    fn extract_load_columns(call: &ast::ExprCall) -> Option<Vec<String>> {
        for keyword in &call.arguments.keywords {
            let kw_name = keyword.arg.as_ref().map(|s| s.as_str());
            match kw_name {
                Some("usecols") | Some("columns") => {
                    if let Some(cols) = Self::extract_string_list(&keyword.value) {
                        return Some(cols);
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
                            return Some(keys);
                        }
                    }
                }
                _ => {}
            }
        }
        None
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
                let schema_display = if schema_name.starts_with("__inferred_") {
                    format!("inferred column set (defined at line {})", defined_line)
                } else {
                    format!("{} (defined at line {})", schema_name, defined_line)
                };
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
        let schema_display = if schema_name.starts_with("__inferred_") {
            format!("inferred column set (defined at line {})", def_line)
        } else {
            format!("{} (defined at line {})", schema_name, def_line)
        };
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
                }
            }
            Stmt::FunctionDef(func_def) => {
                // Track return type annotations like -> PandasFrame[Schema]
                if let Some(returns) = &func_def.returns {
                    if let Some(schema_name) = Self::extract_schema_from_annotation(returns) {
                        self.functions
                            .insert(func_def.name.to_string(), schema_name.to_string());
                    }
                }
                for body_stmt in &func_def.body {
                    self.visit_stmt(body_stmt, errors);
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
                                    if let Some(columns) = self.schemas.get_mut(&schema_name) {
                                        if !columns.iter().any(|c| c == col_name) {
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
                                                let schema_display =
                                                    if base_schema.starts_with("__inferred_") {
                                                        format!(
                                                        "inferred column set (defined at line {})",
                                                        base_def_line
                                                    )
                                                    } else {
                                                        format!(
                                                            "{} (defined at line {})",
                                                            base_schema, base_def_line
                                                        )
                                                    };
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
                                        // pd.read_csv() / pl.scan_parquet() etc.
                                        match Self::extract_load_columns(call) {
                                            Some(cols) => {
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
                                                    message: "columns unknown at lint time; \
                                                              specify `usecols`/`columns` or \
                                                              annotate: `df: Annotated[pd.DataFrame, MySchema] \
                                                              = pd.read_csv(...)`"
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
                                                                if s.starts_with("__inferred_") {
                                                                    format!("inferred column set (defined at line {})", l)
                                                                } else {
                                                                    format!("{} (defined at line {})", s, l)
                                                                }
                                                            })
                                                            .unwrap_or_else(|| "unknown".to_string());
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
                                                        .map(|(s, l)| {
                                                            if s.starts_with("__inferred_") {
                                                                format!("inferred column set (defined at line {})", l)
                                                            } else {
                                                                format!("{} (defined at line {})", s, l)
                                                            }
                                                        })
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
                                    match (base_cols, mapping) {
                                        (Some(base_cols), Some(mapping)) => {
                                            let schema_display = base_info
                                                .as_ref()
                                                .map(|(s, l)| {
                                                    if s.starts_with("__inferred_") {
                                                        format!(
                                                            "inferred column set (defined at line {})",
                                                            l
                                                        )
                                                    } else {
                                                        format!("{} (defined at line {})", s, l)
                                                    }
                                                })
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
                        Expr::Name(name) => {
                            if name.id.as_str() == "concat" {
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
                                } else if let Some(keyword) =
                                    call.arguments.keywords.iter().find(|k| {
                                        k.arg.as_ref().map(|s| s.as_str()) == Some("objs")
                                    })
                                {
                                    if let Expr::List(list) = &keyword.value {
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
                        }
                    } else if let Expr::Name(func_name) = &*call.func {
                        // Handle df = load_users() where load_users() -> PandasFrame[Schema]
                        if let Some(schema_name) = self.functions.get(func_name.id.as_str()) {
                            let schema_name = schema_name.clone();
                            for target in &assign.targets {
                                if let Expr::Name(target_name) = target {
                                    self.variables.insert(
                                        target_name.id.to_string(),
                                        (schema_name.clone(), current_line),
                                    );
                                }
                            }
                        }
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
                        }
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
                            if !columns.contains(&attr_name.to_string())
                                && !RESERVED_METHODS.contains(&attr_name)
                            {
                                let (line, col) = self.source_location(attr.range().start());
                                let schema_display = if schema_name.starts_with("__inferred_") {
                                    format!(
                                        "inferred column set (defined at line {})",
                                        defined_line
                                    )
                                } else {
                                    format!("{} (defined at line {})", schema_name, defined_line)
                                };
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
                                if !columns.iter().any(|c| c == col_name) {
                                    let (line, col) =
                                        self.source_location(subscript.range().start());
                                    let schema_display = if schema_name.starts_with("__inferred_") {
                                        format!(
                                            "inferred column set (defined at line {})",
                                            defined_line
                                        )
                                    } else {
                                        format!(
                                            "{} (defined at line {})",
                                            schema_name, defined_line
                                        )
                                    };
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
                // When the callee is `receiver.method(...)`, do not check the method name
                // as a column access — only recurse into the receiver so that any column
                // accesses nested there (e.g. `df.col.method()`) are still found.
                if let Expr::Attribute(attr) = &*call.func {
                    self.visit_expr(&attr.value, errors);
                } else {
                    self.visit_expr(&call.func, errors);
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
}

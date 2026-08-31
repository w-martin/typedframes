//! Static lookup tables consulted by the linter and the project indexer.
//!
//! Pure data — no logic lives here.

// Directories that should never be descended into when collecting `.py` files by
// default: VCS metadata, virtualenvs, caches, editor/tool state, and vendored/build
// trees. Mirrors ruff's default exclude list (and cli.py's own `_EXCLUDED_DIRS`,
// which MUST be kept in sync with this) -- including ruff's own override semantics:
// `[tool.typedframes] exclude` in pyproject.toml REPLACES this set entirely rather
// than adding to it (see `collect_py_files`).
pub(crate) const DEFAULT_EXCLUDED_DIRS: &[&str] = &[
    ".bzr",
    ".claude",
    ".direnv",
    ".eggs",
    ".git",
    ".git-rewrite",
    ".hg",
    ".ipynb_checkpoints",
    ".mypy_cache",
    ".nox",
    ".pants.d",
    ".pytest_cache",
    ".pytype",
    ".ruff_cache",
    ".svn",
    ".tox",
    ".venv",
    ".vscode",
    ".idea",
    "__pycache__",
    "_build",
    "buck-out",
    "build",
    "dist",
    "node_modules",
    "site-packages",
    "venv",
];

// Reserved pandas/polars method names that shouldn't be used as column names
pub(crate) const RESERVED_METHODS: &[&str] = &[
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

pub(crate) const LOAD_FUNCTIONS: &[&str] = &[
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

// DataFrame-library namespaces whose `LOAD_FUNCTIONS` calls seed a column set. Matched
// against the call receiver flattened by `ast_extract::dotted_module_path`, which is why
// the dotted `dask.dataframe` spelling can sit in the same list as the bare aliases: a
// plain `Expr::Name` receiver flattens to itself, so `pd`/`pl` behave exactly as before.
//
// `dd`/`dask.dataframe` is EXPERIMENTAL. dask.dataframe is a deliberate near-drop-in for
// pandas, so every load function it implements (`read_csv`, `read_parquet`, `read_json`,
// `read_sql`/`read_sql_query`/`read_sql_table`, `read_hdf`, `read_orc`) already appears
// in `LOAD_FUNCTIONS` under its pandas spelling, with the same `usecols=`/`columns=`
// kwargs -- verified by executing them against dask 2026.8.0, not just read from docs.
// The functions dask does NOT have (`read_excel`, `read_html`, `read_feather`,
// `read_clipboard`, polars' `scan_*`/`read_database*`) simply never match a real `dd.`
// call, so no per-module gating is needed.
pub(crate) const LOAD_MODULES: &[&str] = &["pd", "pandas", "pl", "polars", "dd", "dask.dataframe"];

// Module-level constructors that re-wrap an ALREADY-TRACKED frame in another backend's
// container without touching its column set, with the source frame as the first
// positional argument: `dd.from_pandas(pdf, npartitions=2)` -- dask's canonical entry
// point from an in-memory frame -- and `pl.from_pandas(pdf)`. Both verified to return
// the source's columns unchanged by running them, against dask 2026.8.0 and polars
// 1.43.2.
//
// Deliberately excludes dask's other `from_*` constructors: `from_dict` takes a literal
// rather than a tracked frame (the same shape as `pd.DataFrame({...})`, which this
// checker also leaves untracked), and `from_delayed`/`from_array`/`from_map` produce
// columns that only exist at runtime.
pub(crate) const FRAME_WRAP_FUNCTIONS: &[&str] = &["from_pandas"];

// Load functions whose column set lives in a SQL SELECT list rather than a
// usecols/columns/dtype/schema kwarg. `read_sql_table` is deliberately excluded: its
// first positional argument is a table name, not SQL, so attempting to parse it as SQL
// would just fail (harmlessly, but for the wrong reason) rather than being skipped
// because we know better.
pub(crate) const SQL_LOAD_FUNCTIONS: &[&str] = &[
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
pub(crate) const FEAST_RETRIEVAL_METHODS: &[&str] =
    &["get_historical_features", "get_online_features"];

// The `connectorx` package (conventionally imported `as cx`) exposes a `read_sql`
// function with the SQL text as its SECOND positional argument
// (`cx.read_sql(conn_uri, sql)`) — the reverse of pandas' `pd.read_sql(sql, conn)` —
// so it needs its own argument-position handling rather than reusing
// `extract_sql_literal`. Its own module list, separate from `LOAD_MODULES`
// (pd/pl), since it isn't a DataFrame-library namespace itself.
pub(crate) const CONNECTORX_MODULES: &[&str] = &["connectorx", "cx"];

// DataFrame-materializing method that finalizes a connector call chain into an actual
// DataFrame: google-cloud-bigquery's `.to_dataframe()`, PySpark/Databricks Connect's
// `.toPandas()`, DuckDB's `.df()`/`.pl()`. Only dispatches when the call it's chained
// onto is confirmed to be one of `SQL_PRODUCING_METHODS` — see
// `sql_producing_call_args`, and its call sites for why "any receiver's `.df()`" isn't
// enough on its own (that name in particular is common and unrelated most of the time).
pub(crate) const SQL_FINALIZE_METHODS: &[&str] = &["to_dataframe", "toPandas", "df", "pl"];

// Methods/bare functions whose first positional (or `sql=`/`query=` keyword) argument
// is SQL text, chained into one of `SQL_FINALIZE_METHODS`: `client.query(sql)`
// (BigQuery), `spark.sql(sql)`/`session.sql(sql)` (PySpark/Databricks Connect),
// `duckdb.sql(sql)`/`duckdb.query(sql)`.
pub(crate) const SQL_PRODUCING_METHODS: &[&str] = &["query", "sql"];

// Sentinel value used in place of a real schema name wherever a function/method's
// return type is recognized as a bare `pd.DataFrame`/`pl.DataFrame` -- no attached
// Schema (see `ast_extract::extract_bare_dataframe_type`). These maps otherwise hold a
// real name that indexes into `self.schemas`/`ProjectIndex.all_schemas`; this marker
// is never inserted there, so a lookup with it MUST be special-cased by the caller to
// synthesize an open/empty schema (mirroring `register_feast_dataframe`) rather than
// treated as a schema name to resolve. Kept as a plain string rather than widening
// every one of these `HashMap<String, String>` fields to an enum, matching this
// checker's existing "empty string = none" sentinel convention for the same fields.
pub(crate) const OPEN_FRAME_MARKER: &str = "__typedframes_open_frame__";

pub(crate) const ROW_PASSTHROUGH_METHODS: &[&str] = &[
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
    // Materialization and partition-layout changes, which preserve the column set
    // exactly. `compute`/`persist`/`repartition` are dask.dataframe's (all three
    // verified to return the receiver's columns unchanged against dask 2026.8.0);
    // `collect` is polars' LazyFrame finalizer, which had no handling before, so a
    // `pl.scan_csv(...)` schema used to be lost the moment it was collected. Each only
    // fires when the receiver is already a tracked frame, so an unrelated `.collect()`
    // on some other object is still ignored.
    "compute",
    "persist",
    "repartition",
    "collect",
];

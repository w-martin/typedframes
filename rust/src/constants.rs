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

pub(crate) const LOAD_MODULES: &[&str] = &["pd", "pandas", "pl", "polars"];

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
];

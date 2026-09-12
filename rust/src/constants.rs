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
];

// ──────────────────────────────────────────────────────────────────────────────
// PySpark (`pyspark.sql.DataFrame`) — experimental native tracking
// ──────────────────────────────────────────────────────────────────────────────
//
// This is the NATIVE Spark DataFrame path: `spark.read.csv(...)` /
// `spark.createDataFrame(...)` / `spark.sql(...)` kept as a Spark DataFrame and
// chained through `.select()`/`.withColumn()`/… . It is deliberately distinct from
// the pre-existing SQL-connector path (`SQL_PRODUCING_METHODS` /
// `SQL_FINALIZE_METHODS`), which only fires for `spark.sql(sql).toPandas()` — a
// chain that ends in a *pandas* DataFrame. The two never see the same AST node:
// the SQL path matches on the outer `.toPandas()` call, this one on a `.sql()`
// call that is itself the assigned value.

// Module aliases conventionally used for `pyspark.sql.functions`, whose `col("x")`
// is Spark's counterpart to polars' `pl.col("x")`. `F` is the near-universal
// convention (`from pyspark.sql import functions as F`); `sf` and the unaliased
// `functions` are the other two forms seen in the wild. The bare-imported form
// (`from pyspark.sql.functions import col`, then `col("x")`) needs nothing here —
// it is already covered by `ast_extract::extract_pl_col_name`'s backend-agnostic
// bare-`col` branch.
pub(crate) const SPARK_FUNCTIONS_MODULES: &[&str] = &["F", "sf", "functions"];

// `DataFrameReader` methods that terminate a `spark.read…` chain by returning an
// actual DataFrame. Unlike pandas' `read_csv(usecols=[...])` there is no
// column-subset argument here: Spark's column set comes from an explicit
// `.schema(...)`/`schema=` (a `StructType`, a DDL string, or a list of names), and
// is otherwise decided at runtime by Spark's own schema inference — genuinely
// unknowable at lint time, and reported with the same `untracked-dataframe`
// warning a bare `pd.read_csv()` gets.
//
// `text` is deliberately absent: its schema is documented as `value` *plus any
// partition columns*, so neither "unknown" nor a fixed `["value"]` is accurate,
// and it is rare enough not to be worth an open-schema special case.
pub(crate) const SPARK_READ_METHODS: &[&str] = &["csv", "parquet", "json", "orc", "load", "table"];

// `DataFrameReader` methods that return the reader itself, so a `spark.read…`
// chain can be walked through them looking for `.schema(...)`.
pub(crate) const SPARK_READER_CHAIN_METHODS: &[&str] = &["option", "options", "format"];

// Spark DataFrame methods that return a DataFrame with an identical column set:
// row filters, orderings, partitioning/caching hints, and aliasing. Each was
// checked against its PySpark signature and docstring for whether it really is
// column-preserving (which is why `head`/`tail`/`first` are absent — in Spark they
// return `Row`/`list[Row]`, not a DataFrame; see the note in the linter's
// row-passthrough branch).
//
// Kept separate from `ROW_PASSTHROUGH_METHODS` rather than merged into it so the
// Spark additions stay reviewable as a set; both lists are consulted at the same
// call site, and neither is gated on the receiver's backend (the linter tracks a
// variable's column set, not which library produced it).
// PySpark `DataFrame` attributes and methods that are NOT column names, consulted
// only where `df.<name>` attribute access is validated as a column reference (see
// `Linter::visit_expr`). Without this, every `df.write…`, `df.printSchema()` or
// `df.createOrReplaceTempView(...)` on a tracked Spark frame would be reported as an
// unknown column — `RESERVED_METHODS` covers the pandas/polars surface only, and
// Spark's is camelCase and largely disjoint from it.
//
// Deliberately NOT merged into `RESERVED_METHODS`, which is also used for the
// `reserved-name` diagnostic: a pandas user whose schema has a column called `write`
// or `observe` should not start being told it shadows a method that only exists in a
// library they aren't using. The trade-off within Spark is the same one
// `RESERVED_METHODS` already makes — a Spark column genuinely named `where` goes
// unvalidated on attribute access — which is this checker's preferred direction.
pub(crate) const SPARK_RESERVED_ATTRIBUTES: &[&str] = &[
    "write",
    "writeStream",
    "writeTo",
    "na",
    "stat",
    "rdd",
    "sparkSession",
    "isStreaming",
    "isLocal",
    "isEmpty",
    "storageLevel",
    "printSchema",
    "show",
    "take",
    "toDF",
    "toPandas",
    "toArrow",
    "toJSON",
    "toLocalIterator",
    "createTempView",
    "createOrReplaceTempView",
    "createGlobalTempView",
    "createOrReplaceGlobalTempView",
    "registerTempTable",
    "selectExpr",
    "withColumn",
    "withColumns",
    "withColumnRenamed",
    "withColumnsRenamed",
    "withWatermark",
    "where",
    "orderBy",
    "sortWithinPartitions",
    "distinct",
    "dropDuplicates",
    "drop_duplicates",
    "dropDuplicatesWithinWatermark",
    "union",
    "unionAll",
    "unionByName",
    "intersect",
    "intersectAll",
    "exceptAll",
    "subtract",
    "crossJoin",
    "groupBy",
    "rollup",
    "cube",
    "unpivot",
    "colRegex",
    "repartition",
    "repartitionByRange",
    "repartitionById",
    "coalesce",
    "cache",
    "persist",
    "unpersist",
    "checkpoint",
    "localCheckpoint",
    "hint",
    "offset",
    "explain",
    "observe",
    "summary",
    "freqItems",
    "approxQuantile",
    "sampleBy",
    "replace",
    "inputFiles",
    "foreach",
    "foreachPartition",
    "mapInPandas",
    "mapInArrow",
    "toPandasOnSpark",
    "sameSemantics",
    "semanticHash",
];

pub(crate) const SPARK_ROW_PASSTHROUGH_METHODS: &[&str] = &[
    "where",
    "limit",
    "offset",
    "orderBy",
    "distinct",
    "dropDuplicates",
    "drop_duplicates",
    "dropDuplicatesWithinWatermark",
    "sortWithinPartitions",
    "repartition",
    "repartitionByRange",
    "coalesce",
    "cache",
    "persist",
    "unpersist",
    "checkpoint",
    "localCheckpoint",
    "alias",
    "hint",
];

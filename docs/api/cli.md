# CLI

The `typedframes` command-line interface runs the Rust-based static checker on Python source files.

## Basic usage

```shell
# Check a single file
typedframes check pipeline.py

# Check an entire directory (builds a cross-file project index)
typedframes check src/

# Check without building the project index (each file checked independently)
typedframes check src/ --no-index

# Enable untracked-dataframe warnings for bare DataFrame loads (off by default)
typedframes check src/ --strict-ingest

# Output formats
typedframes check src/ --output-format text    # default — ty-style, auto-colored in terminal
typedframes check src/ --output-format json    # machine-readable JSON
typedframes check src/ --output-format github  # GitHub Actions annotations
```

## Supported file formats

The checker reads column information from load calls for all common formats:

```python
# pandas
pd.read_csv("data.csv", usecols=["a", "b"])
pd.read_parquet("data.parquet", columns=["a", "b"])
pd.read_json("data.json", dtype={"a": int, "b": str})
pd.read_excel("data.xlsx", usecols=["a", "b"])

# polars
pl.read_csv("data.csv", columns=["a", "b"])
pl.read_parquet("data.parquet", columns=["a", "b"])
pl.read_json("data.json", schema={"a": int, "b": str})
```

Any `usecols=` (pandas) or `columns=` / `schema=` (polars) argument teaches the checker
which columns are available, regardless of file format.

## SQL and warehouse column inference

For database/warehouse loads, the checker infers columns from the query's `SELECT`
list instead of a kwarg:

```python
pd.read_sql("SELECT a, b FROM t", conn)
pd.read_sql_query("SELECT a, b FROM t", conn)
pl.read_database("SELECT a, b FROM t", connection)
pl.read_database_uri("SELECT a, b FROM t", uri)
pd.read_gbq("SELECT a, b FROM t", project_id=...)
```

This also traces the query text back through:

- a **single-assignment variable** (`QUERY = "SELECT a, b FROM t"`, used later) — a
  variable assigned more than once anywhere in the file is not resolved, since the
  checker can't know which assignment was in effect at the call site;
- a **`.sql` file** (`Path("query.sql").read_text()`, `open("query.sql").read()`,
  project-root-relative only);
- SQLAlchemy's `text(...)` wrapper, `select(Model.col1, Model.col2, ...)` (Core, against
  a declarative model's columns), and connector-specific shapes like
  `client.query(sql).to_dataframe()` (BigQuery), `spark.sql(sql).toPandas()`
  (PySpark/Databricks), `duckdb.sql(sql).df()`, and the `cursor.execute(sql)` /
  `cursor.fetch_pandas_all()` pattern (Snowflake, Redshift).

An f-string or `.format()`-built query is deliberately **not** resolved — that's the SQL
injection anti-pattern parameterized queries exist to avoid — and falls through to
`untracked-dataframe` like any other unresolvable load, without a separate injection
warning (the checker has no taint analysis to distinguish a safe interpolation from a
real vulnerability).

Set `sql_dialect` in `pyproject.toml` to fold identifier case the way a specific engine
does (e.g. Snowflake upper-cases unquoted identifiers, Postgres/Redshift lower-case
them) — see [Project-level configuration](#project-level-configuration). Full examples
for eleven connectors, including Feast and SQLAlchemy, live in the repo's
`examples/sql_connectors/` directory (`snowflake/`, `bigquery/`, `athena/`, `redshift/`,
`databricks/`, `pyspark/`, `duckdb/`, `connectorx/`, `sqlalchemy/`, `feast/`,
`azure_synapse/`).

A wrapper function that queries one of these connectors and then case-folds the
result (e.g. an internal package that queries Snowflake and lower-cases its
genuinely-upper-cased columns before returning) is also traced, cross-file, via
`.rename(columns=str.lower)` / `df.columns = df.columns.str.lower()` (and the
`.upper()` equivalents) — see [usage.md's "Supported column-set
transforms"](../usage.md#supported-column-set-transforms) for exactly what's
recognized and why arbitrary custom transform functions aren't.

## Output format

```
src/pipeline.py:42:8: error[unknown-column] Column 'revenue' not in OrderSchema
src/pipeline.py:57:8: error[reserved-name] Column 'user_id' renamed to 'customer_id', use 'customer_id'
src/pipeline.py:10:1: warning[untracked-dataframe] columns unknown at lint time; specify usecols= or annotate
src/pipeline.py:12:5: error[missing-column] 'customers' passed to contact_label (transforms.py:2) is missing
  column(s) {email} — available: {customer_id, name, region}, required: {email, name}
```

The format matches ty and ruff: `file:line:col: severity[code] message`. Most editors,
CI systems, and LSP clients parse this automatically. Colors are applied when the output
is a terminal (TTY); piping or redirecting strips them.

## Error codes

| Code | Meaning | Default |
|------|---------|---------|
| `unknown-column` | Column not found in schema or inferred set | Always shown |
| `reserved-name` | Column was renamed — use the new name | Always shown |
| `untracked-dataframe` | Bare DataFrame load — no column info for checker | Off (use `--strict-ingest`) |
| `dropped-unknown-column` | Dropped column doesn't exist in schema | Off (use `--strict-ingest`) |
| `missing-column` | Argument's columns don't satisfy the called function's parameter contract | Always shown |

## Project-level configuration

Add to `pyproject.toml` to disable all warnings project-wide:

```toml
[tool.typedframes]
enabled  = true
warnings = false
```

Set `sql_dialect` to fold unquoted SQL identifier case the way a specific engine does
when inferring columns from a `SELECT` list (see [SQL and warehouse column
inference](#sql-and-warehouse-column-inference)). Unset or unrecognized values default
to no folding (columns keep the exact case written in the query).

```toml
[tool.typedframes]
sql_dialect = "snowflake"  # one of: bigquery, snowflake, redshift, databricks,
                           # duckdb, postgres, mysql, hive, spark, mssql
                           # (mssql aliases: sqlserver, synapse, fabric)
```

SQL parsing also accepts T-SQL's `[bracket-quoted]` identifiers (SQL Server, Azure SQL,
Synapse, Fabric Warehouse) alongside standard `"double-quoted"` ones, regardless of
`sql_dialect`.

---

::: typedframes.cli.main

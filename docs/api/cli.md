# CLI

The `typedframes` command-line interface runs the Rust-based static checker on Python source files.

## Basic usage

```shell
# Check a single file
typedframes check pipeline.py

# Check an entire directory (builds a cross-file project index)
typedframes check src/
```

Every other flag is documented below, grouped by what it controls, each with a runnable
example. `typedframes check <path>` alone runs with every default: cross-file index on,
warnings on, text output, no coverage gate.

## Flags reference

### Where to check

- **`path`** (positional, required) — file or directory to check.
- **`--no-index`** — skip building the cross-file project index; check each file in
  isolation. Faster for a single file, but blind to imports across files (a schema
  defined in `schemas.py` and used in `pipeline.py` won't be followed).

  ```shell
  typedframes check src/pipeline.py --no-index
  ```

### What to report

- **`--no-warnings`** — suppress `untracked-dataframe` and `dropped-unknown-column`
  diagnostics. They still count toward DataFrame schema coverage either way — this only
  silences the printed warning, not the underlying "columns unknown" fact.

  ```shell
  typedframes check src/ --no-warnings
  ```

- **`--lenient-ingest`** — downgrade `untracked-dataframe` from a warning to an info-level
  note, for EDA-style code that loads data before knowing its shape on purpose.

  ```shell
  typedframes check src/ --lenient-ingest
  ```

- **`--no-info`** — silence the informational one-line summary (files checked, elapsed
  time, DataFrame schema coverage ratio). A failed `--coverage-fail-under` gate is still
  reported regardless — this flag only silences the *informational* line, never a result.

  ```shell
  typedframes check src/ --no-info
  ```

- **`--strict`** — exit `1` if any errors were found. Without it, `check` still reports
  errors but exits `0` unless something else (a failed coverage gate) demands non-zero.
  Warnings alone never trigger `--strict`; it only judges errors.

  ```shell
  typedframes check src/ --strict
  ```

### How to shape the output

These two flags are independent axes, not two ways to ask for the same thing:
**`--output-format` picks the shape everything prints in; `--coverage-detail` picks how
much coverage detail is in it.** Neither implies a value for the other.

- **`--output-format {text,json,github}`** (default `text`) — the shape of every printed
  result: diagnostics, the coverage summary line, coverage detail, gate failures.

  ```shell
  typedframes check src/ --output-format text    # default — ty/ruff-style, auto-colored on a TTY
  typedframes check src/ --output-format json    # one JSON document to stdout
  typedframes check src/ --output-format github  # GitHub Actions workflow annotations
  ```

- **`--coverage-detail {summary,term-missing}`** (default `summary`) — how much DataFrame
  schema coverage detail to print, independent of `--output-format`. `summary` is the
  existing one-line ratio (the default — an unconfigured project sees exactly what it
  always saw). `term-missing` adds a breakdown of exactly which DataFrames lack column
  info. There is deliberately no `json` value here: JSON-ness is `--output-format`'s job
  alone. Ask for `term-missing` detail under whichever format you're already using, and
  it renders in that shape — a text table under `text`, or the same data nested under a
  `coverage` key under `json`:

  ```shell
  # As a text table
  typedframes check src/ --coverage-detail term-missing

  # The exact same detail, as structured JSON nested under a "coverage" key
  typedframes check src/ --output-format json --coverage-detail term-missing
  ```

  See [DataFrame schema coverage thresholds](../usage.md#reporting-seeing-whats-missing)
  for the full text-table and JSON payload shapes side by side.

### Enforcing coverage

- **`--coverage-fail-under=N`** — exit `1` if fewer than `N`% of DataFrames had
  recognized column info. A total override: applies one threshold to every file and
  ignores `[tool.typedframes.coverage]` entirely, per-path overrides included — handy for
  a one-off CI run without touching project config.

  ```shell
  typedframes check src/ --coverage-fail-under=90
  ```

  For a threshold that persists across runs, or that needs a lower bar for legacy code,
  configure `[tool.typedframes.coverage]` in `pyproject.toml` instead — see
  [DataFrame schema coverage thresholds](../usage.md#dataframe-schema-coverage-thresholds)
  for per-path overrides, which this flag doesn't support.

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
| `untracked-dataframe` | Bare DataFrame load — no column info for checker | Always shown (use `--lenient-ingest` to downgrade to info) |
| `dropped-unknown-column` | Dropped column doesn't exist in schema | Always shown |
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

### Tracing installed (non-project) packages

Only files under the project itself are indexed by default — nothing inside `.venv` or
any other installed-dependency location — *except* an installed package whose function
your own first-party code calls in a way that looks like a DataFrame source: the
result of an unrecognized call is later subscripted, or has a pandas/polars method
called on it. That package's `Annotated[...]`/`BaseSchema` declarations and recognized
transform patterns (rename, drop, case-fold, etc. — the same fixed set already applied
to first-party code) are then indexed automatically, the same as any project-local
helper. This is what makes a company-internal package (e.g. one that wraps a SQL
connector) traceable with no configuration: your own code calling it in that shape is
the signal.

```python
# your project's own code — no config needed for this to get traced
from internal_snowflake_pkg import load_orders

orders = load_orders(query)  # unrecognized call...
print(orders["order_id"])  # ...but subscripted like a DataFrame — traced automatically
```

Auto-discovery only ever considers packages your own code actually imports and calls
this way — it never scans the whole `site-packages` tree, so pandas/polars/numpy/etc.
being installed (and never having a call site of their own left unresolved, since
`pd.read_csv`-style calls are always recognized directly) never triggers tracing into
their own, often enormous, internal source. Auto-discovered candidates are also capped
at 20 packages per project, so a codebase that happens to call many different external
packages in DataFrame-shaped ways can't balloon index-build cost; `trace_external_packages`
entries are never subject to that cap.

Two settings give you explicit control over this:

```toml
[tool.typedframes]
trace_external_packages = ["internal_snowflake_pkg"]      # force-trace, even if auto-discovery wouldn't catch it
excluded_external_packages = ["some_huge_or_untrusted_pkg"] # opt out, even if auto-discovery would have traced it
```

- `trace_external_packages` always wins — a package named there is traced regardless
  of `excluded_external_packages` or the auto-discovery cap. Use it for a package whose
  usage pattern is too indirect for auto-discovery to catch (e.g. passed as a callback
  rather than called directly).
- `excluded_external_packages` only suppresses *auto-discovered* candidates. It has no
  effect on a package also named in `trace_external_packages`.
- The package's install location is auto-detected from the project's own `.venv`
  (`lib/pythonX.Y/site-packages` on Unix, `Lib/site-packages` on Windows) — there is no
  path override in this version, and no other virtualenv-manager layout is searched.
- Only the resolved packages' own directories are walked — never the whole
  `site-packages` tree.
- **Editable installs are not supported in this version.** A package installed via
  `pip install -e` (or `uv`'s equivalent) resolves through a `.pth`/`direct_url.json`
  redirect rather than living directly under `site-packages`, and isn't found by the
  current auto-detection. Install the package normally (a real, non-editable install)
  for it to be traced.
- Indexing an external package means trusting its source enough to run static analysis
  over it. Auto-discovery only ever adds packages your own project's code demonstrably
  calls in a DataFrame-shaped way, never anything installed but unused this way — but
  if you want a hard guarantee that nothing outside the project is ever indexed without
  an explicit name, list the specific packages to keep out in
  `excluded_external_packages`. There is no single setting to disable auto-discovery
  project-wide in this version; exclusion is by exact package name.

### Excluding directories

The following are pruned by default — no config needed:

```
.bzr  .claude  .direnv  .eggs  .git  .git-rewrite  .hg  .ipynb_checkpoints
.mypy_cache  .nox  .pants.d  .pytest_cache  .pytype  .ruff_cache  .svn  .tox
.venv  .vscode  .idea  __pycache__  _build  buck-out  build  dist
node_modules  site-packages  venv
```

`exclude` **replaces this default list entirely** — matching by bare directory name,
not a path/glob pattern:

```toml
[tool.typedframes]
exclude = [".claude", "legacy"]
```

This mirrors ruff's own `exclude` (there's no separate `extend-exclude`-style option
here): once `exclude` is set, only the names you list are pruned, so re-list `.venv`
(or anything else from the default set you still want ignored) alongside your own
additions, or it'll be walked. An explicit `exclude = []` is a deliberate way to prune
nothing at all.

Applies both when checking a directory directly (`typedframes check .`) and to the
cross-file project index — a single `exclude` value controls both.

---

::: typedframes.cli.main

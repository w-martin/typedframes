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

# Downgrade untracked-dataframe from a warning to a quiet info-level note for bare
# DataFrame loads (warning by default)
typedframes check src/ --lenient-ingest

# Output formats
typedframes check src/ --output-format text    # default — ty-style, auto-colored in terminal
typedframes check src/ --output-format json    # machine-readable JSON
typedframes check src/ --output-format github  # GitHub Actions annotations

# Fail on any error — for CI (errors always exit non-zero standalone; --strict
# additionally fails a run that only produced warnings)
typedframes check src/ --strict

# Suppress all warnings (untracked-dataframe, dropped-unknown-column)
typedframes check src/ --no-warnings

# Silence the informational summary line (a failed gate is still reported)
typedframes check src/ --no-info

# Enforce a minimum DataFrame schema coverage threshold — see
# [DataFrame schema coverage thresholds](../usage.md#dataframe-schema-coverage-thresholds)
typedframes check src/ --fail-under=90

# Show which DataFrames lack column info, per file ("term-missing") or as JSON
typedframes check src/ --coverage-report=term-missing
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
any other installed-dependency location — *except* an installed package that gets
traced in on demand, by one of two independent routes:

1. **Behavioral discovery** (works for any package, typed or not): your own first-party
   code calls it in a way that looks like a DataFrame source, and the result is used
   like a DataFrame afterward.
2. **`py.typed` trust** (only for packages that declare it — see below): your own code
   calls it through a tracked `self.<attr>` — no usage confirmation needed, since the
   package's own declared return type is authoritative.

Either route indexes that package's source the same way a project-local file is
indexed: its `Annotated[...]`/`BaseSchema` declarations, recognized transform patterns
(rename, drop, case-fold, etc.), and — see [Bare return
types](#bare-pddataframepldataframe-return-types) below — plain `pd.DataFrame`/
`pl.DataFrame` return annotations are all read from it directly.

#### Behavioral discovery: a call whose result is used like a DataFrame

The result of an unrecognized call — a bare function, `module.func(...)`, or
`self.<attr>.<method>(...)` on an instance set up in `__init__` — is later subscripted
or has a pandas/polars method called on it:

```python
# your project's own code — no config needed for this to get traced
from internal_snowflake_pkg import load_orders

orders = load_orders(query)  # unrecognized call...
print(orders["order_id"])  # ...but subscripted like a DataFrame — traced automatically
```

The same applies to a repository/wrapper-class pattern — a very common shape for
internal packages that wrap a database connector — as long as the wrapper's result is
used somewhere:

```python
# your project's own code
from internal_snowflake_pkg import DataRepository


class Pipeline:
    def __init__(self):
        self._data_repository = DataRepository()  # tracked: attr -> class, in __init__

    def run(self):
        df = self._data_repository.get_training(query)  # unrecognized call...
        print(df["feature_a"])  # ...but subscripted like a DataFrame — traced automatically
```

This route works for **any** installed package, regardless of whether it ships type
annotations — it's a fallback that asks "was this actually used like a DataFrame?"
rather than trusting a declared type.

#### `py.typed` trust: no usage confirmation needed

A package that ships a [PEP 561](https://peps.python.org/pep-0561/) `py.typed` marker
is declaring that its own type annotations are meant to be trusted by tooling. When a
tracked `self.<attr>.<method>(...)` call's `<attr>` resolves to a class from a
`py.typed` package, typedframes traces it **immediately** — the package's own return
annotation is read directly, without needing to see the result used like a DataFrame
anywhere first:

```python
# your project's own code — self._data_repository is never subscripted, printed, or
# passed anywhere DataFrame-shaped in this file. Still traced, because
# internal_repo_pkg/py.typed exists.
from internal_repo_pkg import DataRepository


class Pipeline:
    def __init__(self):
        self._data_repository = DataRepository()

    def run(self):
        df = self._data_repository.get(query)
        return df  # never used as a DataFrame here -- doesn't matter
```

Only `self.<attr>.<method>(...)` calls go through this route today (not bare or
`module.func()` calls) — matching the shape most repository/wrapper packages actually
use. A package without `py.typed` isn't penalized for it; it simply falls back to
behavioral discovery, exactly as it always has.

#### Bare `pd.DataFrame`/`pl.DataFrame` return types

Most third-party packages have no reason to know about this project's `Schema`
classes, so their return annotations — `py.typed`-declared or not — are almost always
a bare `pd.DataFrame`/`pl.DataFrame`, never `Annotated[pd.DataFrame, YourSchema]`. A
bare DataFrame return type is still registered as a resolved DataFrame origin, with an
**open schema**: it counts toward [DataFrame schema
coverage](../usage.md#dataframe-schema-coverage-thresholds) (this checker knows it's a
DataFrame), but no specific column is ever flagged as unknown against it (this checker
doesn't know *which* columns), the same treatment a Feast retrieval's unresolvable
columns already get. If the package's real source is available and its function's body
resolves a concrete column set (e.g. a `usecols=`-style load, or a SQL `SELECT` list),
that's used instead — a bare return annotation is the lowest-priority fallback, never a
ceiling on precision.

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
  for it to be traced. This applies to `py.typed` detection too: the marker file is
  looked for at `<site-packages>/<package>/py.typed`, so an editable install's `py.typed`
  isn't found any more than its source is.
- A `py.typed` package is still subject to the same 20-package auto-discovery cap and
  `excluded_external_packages` opt-out as any other auto-discovered candidate — the
  only thing `py.typed` changes is that behavioral confirmation (a subscript or
  pandas/polars method call on the result) isn't required first, for the
  `self.<attr>.<method>(...)` shape specifically.
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

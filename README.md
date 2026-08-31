# typedframes

[![CI](https://github.com/w-martin/typedframes/actions/workflows/publish.yml/badge.svg)](https://github.com/w-martin/typedframes/actions/workflows/publish.yml)
[![PyPI version](https://img.shields.io/pypi/v/typedframes.svg)](https://pypi.org/project/typedframes/)
[![Python versions](https://img.shields.io/pypi/pyversions/typedframes.svg)](https://pypi.org/project/typedframes/)
[![Coverage](https://coveralls.io/repos/github/w-martin/typedframes/badge.svg?branch=main)](https://coveralls.io/github/w-martin/typedframes?branch=main)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

> ⚠️ **Project Status: Proof of Concept**
>
> `typedframes` (v0.5.0) is currently an experimental proof-of-concept. The core static analysis and mypy/Rust
> integrations work, but expect rough edges. The codebase prioritizes demonstrating the viability of static DataFrame
> column checking over production-grade stability.
>
**A Rust-fast linter for pandas and polars DataFrames. Catches column errors at lint-time, and gates CI on how
much of your codebase it can actually see — no schema classes required to start.**

```python
import pandas as pd

# Checker infers {order_id, amount, status} from usecols= — no schema class needed
orders = pd.read_csv("orders.csv", usecols=["order_id", "amount", "status"])
print(orders["amount"])  # ✓ OK
print(orders["revenue"])  # ✗ unknown-column — 'revenue' not in inferred column set
```

```shell
typedframes check src/ --coverage-fail-under=90
# src/pipeline.py:7:8: error[unknown-column] Column 'revenue' does not exist in inferred column set (defined at line 6)
# ✗ Found 1 error in 12 files (0.0s)
# ✗ DataFrame schema coverage 82.0% is below the required 90.0% (9/11 DataFrames had column info)
```

`--coverage-fail-under=N` is the same idea as a test-coverage or mypy type-coverage gate, applied to how many DataFrames
the checker can resolve columns for — see [DataFrame Schema Coverage Thresholds](#dataframe-schema-coverage-thresholds-opt-in).
Add `BaseSchema` classes later for cross-file awareness and IDE autocomplete — see [Quick Start](#quick-start).

---

## Table of Contents

- [Why typedframes?](#why-typedframes)
- [Installation](#installation)
- [Quick Start](#quick-start)
- [Column Inference](#column-inference)
- [Static Analysis](#static-analysis)
- [DataFrame Schema Coverage Thresholds](#dataframe-schema-coverage-thresholds-opt-in)
- [Static Analysis Performance](#static-analysis-performance)
- [Type Safety With Multiple Backends](#type-safety-with-multiple-backends)
- [Advanced Usage](#advanced-usage)
- [Comparison](#comparison)
- [Pandera Integration](#pandera-integration)
- [Examples](#examples)
- [Philosophy](#philosophy)
- [FAQ](#faq)

---

## Why typedframes?

**The problem:** Many pandas bugs are column mismatches — you access a column that doesn't exist, pass a
DataFrame missing a column a function needs, or make a typo. These errors only surface at runtime, often in
production, and it's hard to know how much of a codebase is actually protected against them.

**The solution:** A fast standalone linter that infers column sets from your existing code (`usecols=`,
`dtype=`, method chains) and catches mismatches at lint-time — plus an opt-in coverage threshold so CI fails
when too much of the codebase is invisible to the checker. Add `BaseSchema` classes where you want cross-file
tracking and IDE autocomplete; they're a progressive enhancement, not a prerequisite.

**What you get:**

- ✅ **Works without schema annotations** - Column inference from `usecols=`, `dtype=`, and method chains catches errors on unannotated code
- ✅ **CI gate via coverage threshold** - `--coverage-fail-under=N` fails the build when too much of your codebase is invisible to the checker, the same way you'd gate on test coverage or a type checker's type-coverage number
- ✅ **Rust-fast** - Milliseconds, not seconds, even on hundreds of files; fast enough for pre-commit hooks and CI (see [benchmarks](#static-analysis-performance))
- ✅ **Cross-file awareness** - Add `BaseSchema` and typed return annotations to follow schemas across module boundaries
- ✅ **Refactor-safe access** - `df[Schema.column_group.s].mean()` (pandas) or `df.select(Schema.col.col)` (polars) instead of scattered string literals
- ✅ **Works with pandas AND polars** - Same schema API, native backend types
- ✅ **Dynamic column matching** - Regex-based ColumnSets for time-series data
- ✅ **Zero runtime overhead** - No validation, no slowdown
- ✅ **Type-safe backends** - Type checker knows pandas vs polars methods

---

## Installation

```shell
pip install typedframes
```
or
```shell
uv add typedframes
```

The Rust-based checker is included — no separate install needed. `typedframes` itself has
no required dependencies; add the extra for whichever backend(s) your code uses:

```shell
pip install typedframes[pandas]   # includes pandas
pip install typedframes[polars]   # includes polars
```

---

## Quick Start

### Run on existing code

The checker works from day one without any schema classes. Pass `usecols=` / `columns=` to your read calls and
column access is validated automatically — no schema classes needed:

```python
import pandas as pd

# Checker infers {order_id, amount, status} from usecols=
orders = pd.read_csv("orders.csv", usecols=["order_id", "amount", "status"])
print(orders["amount"])  # ✓ OK
print(orders["revenue"])  # ✗ unknown-column — 'revenue' not in inferred set
```

```shell
typedframes check src/
# src/pipeline.py:7:8: error[unknown-column] Column 'revenue' does not exist in inferred column set (defined at line 6)
# ✗ Found 1 error in 12 files (0.0s)
```

See [`examples/features/multi_file_inference/`](examples/features/multi_file_inference/) for a multi-file example with no `BaseSchema`
classes at all.

### Define Your Schema (Once)

Add `BaseSchema` classes when you want cross-file awareness and IDE autocomplete. Schemas travel with function return
types across module boundaries — the checker validates call sites even in files that have no `usecols=` of their own.

**Descriptors as a bridge:** define once in `Column(type=int)`, access as `df[UserData.user_id.s]` (pandas string
access) or `df.select(UserData.revenue.col)` (polars expression). Refactor by changing the descriptor definition —
all `.s` and `.col` references update automatically. No find-and-replace across string literals.

```python
from typedframes import BaseSchema, Column, ColumnSet


class SalesData(BaseSchema):
    date = Column(type=str)
    revenue = Column(type=float)
    customer_id = Column(type=int)

    # Dynamic columns with regex
    metrics = ColumnSet(type=float, members=r"metric_\d+", regex=True)
```

### Use With Pandas

```python
from typing import Annotated
import pandas as pd

# Annotate your variable — checker validates all column access below
df: Annotated[pd.DataFrame, SalesData] = pd.read_csv("sales.csv")

# String access — validated by the standalone checker
print(df["revenue"].sum())
print(df["profit"])  # ✗ unknown-column: Column 'profit' not in SalesData

# .s gives a refactor-safe string name from the descriptor
print(df[SalesData.revenue.s].sum())  # same as df['revenue'].sum()


# Type-safe function signature
def analyze(data: Annotated[pd.DataFrame, SalesData]) -> float:
    data["revenue"]  # ✓ Validated by checker
    data["profit"]  # ✗ unknown-column: 'profit' not in SalesData
    return data[SalesData.revenue.s].mean()
```

### Use With Polars

```python
from typing import Annotated
import polars as pl

# Annotate your variable — checker validates pl.col() references too
df: Annotated[pl.DataFrame, SalesData] = pl.read_csv("sales.csv")

# pl.col() references are now validated by the standalone checker
print(df.filter(pl.col("revenue") > 1000))
print(df.select(pl.col("profit")))  # ✗ unknown-column: Column 'profit' not in SalesData

# .col gives a refactor-safe polars expression from the descriptor
filtered = df.filter(SalesData.revenue.col > 1000)
grouped = df.group_by("customer_id").agg(SalesData.revenue.col.sum())
```

---

## Column Inference

The standalone checker works without any `BaseSchema` classes.  It infers column sets directly from data
loading calls and method chains, so you get column validation even on completely unannotated code.
`BaseSchema` is a progressive enhancement: it adds cross-file awareness and IDE autocomplete, but the
checker catches real bugs from day one without it.

### Inferred Schemas

When you pass `usecols=` (pandas) or `schema=` / `columns=` (polars), the checker builds an inferred column set
and validates all subscript access against it — no schema annotation required:

```python
# Checker infers {user_id, email} from usecols= — no annotation needed
df = pd.read_csv("users.csv", usecols=["user_id", "email"])
print(df["user_id"])  # ✓ OK — in usecols
# print(df["age"])     # ✗ Error: 'age' not in inferred column set
```

The checker also propagates column sets through method chains. Row-preserving operations (`filter`, `query`,
`head`, `tail`, `sort_values`, `dropna`, `fillna`, `ffill`, `bfill`, `reset_index`) pass the column set through
unchanged. Structural operations update it:

```python
from typing import Annotated

df: Annotated[pd.DataFrame, UserData] = pd.read_csv("users.csv")

# Subscript slice — inferred column set {user_id, email}
small = df[["user_id", "email"]]
# print(small["age"])  # ✗ Error: 'age' not in inferred column set

# rename() — old name removed, new name added
renamed = small.rename(columns={"email": "email_address"})
print(renamed["email_address"])  # ✓ OK

# drop() — column removed from inferred set
trimmed = df.drop(columns=["age"])
# print(trimmed["age"])  # ✗ Error: 'age' was dropped

# assign() — new column added to inferred set
augmented = df.assign(created_at="2024-01-01")
print(augmented["created_at"])  # ✓ OK
```

### Inference Gaps and Warnings

**untracked-dataframe — unannotated data ingestion (on by default)**

When a DataFrame is loaded via `pd.read_csv()` without `usecols=` or a schema annotation, the checker
assumes an *Unknown* state, bypasses strict column validation on it (to avoid false positives on columns
it simply can't see), and flags the load itself as a warning-level diagnostic.

For permissive Exploratory Data Analysis (EDA) work where you don't want that noise yet, downgrade it to
a quiet info-level note with `--lenient-ingest`:

```shell
typedframes check src/ --lenient-ingest
```

By default, loading a DataFrame without a schema or `usecols=` produces:

```python
df = pd.read_csv("users.csv")
# ⚠ untracked-dataframe: columns unknown at lint time; specify `usecols`/`columns`, or
#   annotate the variable's type, e.g. `df: Annotated[pd.DataFrame, MySchema] = pd.read_csv(...)`
```

Fix option 1 — annotate with a schema:
```python
from typing import Annotated

df: Annotated[pd.DataFrame, UserData] = pd.read_csv("users.csv")
```

Fix option 2 — pass `usecols=`:
```python
df = pd.read_csv("users.csv", usecols=["user_id", "email"])
```

**dropped-unknown-column — dropped column does not exist**

Emitted when `drop(columns=[...])` names a column that isn't in the inferred set:

```python
from typing import Annotated

df: Annotated[pd.DataFrame, UserData] = pd.read_csv("users.csv")
trimmed = df.drop(columns=["nonexistent"])
# ⚠ dropped-unknown-column: Dropped column 'nonexistent' does not exist in UserData
```

### Function Parameter Contracts

Beyond validating access at the point it happens, the checker infers a *contract* for any function's
first parameter: every column the function needs, drawn from what its body accesses or — taking priority —
from a schema annotation on the parameter itself. Calling that function with a DataFrame that doesn't
satisfy the contract is caught at the **call site**, across files:

```python
# transforms.py
def contact_label(customers):
    return customers["name"] + customers["email"]
```

```python
# pipeline.py
customers = load_customers(path)  # inferred columns: {customer_id, name, region}
contact_label(customers)
# ✗ missing-column: 'customers' passed to contact_label (transforms.py:2) is missing
#   column(s) {email} — available: {customer_id, name, region}, required: {email, name}
```

The contract is resolved *transitively*: if a function only forwards its parameter to other functions
(`step1 = preprocess(df); step2 = enrich(step1)`), the checker follows the chain and unions their
requirements, catching a missing column even when no single function in the chain touches it directly.
Column-list slices (`df[["a", "b"]]`) contribute to the contract too.

**Known limitations:**
- Cross-file delegate/schema resolution follows `from module import name`, plain `import module` +
  `module.helper(df)` attribute access, and `from module import *` wildcard imports. A dotted import with no
  alias (`import a.b.c`) only binds the first segment (`a`), matching Python's own binding rules, so a
  deeply nested submodule accessed without an alias is not tracked.
- Contract inference is a single top-to-bottom pass over a function body, not full control-flow analysis.
  Deeply nested control flow (nested `try`/`except`, `match`, comprehensions) may under-report a function's
  true requirements.
- A cycle in the delegate graph (mutually- or self-delegating helpers) contributes only each function's own
  direct requirements to the cycle, not the full transitive union — conservative rather than exhaustive.
- If two plainly-imported modules both define a same-named function, an attribute-style delegate call
  (`module.helper(df)`) resolves to whichever one is found first — the checker doesn't disambiguate by which
  module the call site actually used.

### See Also

- [`examples/features/inference_example.py`](examples/features/inference_example.py) — single-file walkthrough of all four inference
  scenarios with annotated ✓/✗ comments.
- [`examples/features/multi_file_inference/`](examples/features/multi_file_inference/) — multi-file project checked with
  `typedframes check examples/features/multi_file_inference/`; no `BaseSchema` anywhere. Includes a function
  parameter contract violation caught at the call site (`missing-column`).
- [`examples/features/multi_file_with_schema/`](examples/features/multi_file_with_schema/) — same scenario with `BaseSchema`
  classes; the checker follows schemas across module boundaries via the project index.
- SQL / data-warehouse column inference — the column set is inferred from a query's `SELECT` list instead of
  `usecols=`/`columns=`, including tracing the query back through a single-assignment variable or a `.sql` file,
  and dialect-aware identifier case folding (`sql_dialect` in `pyproject.toml` — see [Project-level
  configuration](docs/api/cli.md#project-level-configuration)):
  [`examples/sql_connectors/snowflake/`](examples/sql_connectors/snowflake/), [`examples/sql_connectors/bigquery/`](examples/sql_connectors/bigquery/),
  [`examples/sql_connectors/athena/`](examples/sql_connectors/athena/), [`examples/sql_connectors/redshift/`](examples/sql_connectors/redshift/),
  [`examples/sql_connectors/databricks/`](examples/sql_connectors/databricks/), [`examples/sql_connectors/pyspark/`](examples/sql_connectors/pyspark/),
  [`examples/sql_connectors/duckdb/`](examples/sql_connectors/duckdb/), [`examples/sql_connectors/connectorx/`](examples/sql_connectors/connectorx/),
  [`examples/sql_connectors/sqlalchemy/`](examples/sql_connectors/sqlalchemy/) (Core `select()` and declarative models, not just raw SQL text),
  [`examples/sql_connectors/feast/`](examples/sql_connectors/feast/) (feature-store retrieval, registered as an *open* schema since
  `entity_df`'s own columns aren't enumerable in general), and
  [`examples/sql_connectors/azure_synapse/`](examples/sql_connectors/azure_synapse/) (Azure's closest analog to Athena, including T-SQL's
  `[bracket-quoted]` identifier convention). A wrapper function that case-folds a connector's result before
  returning it (`.rename(columns=str.lower)`, `df.columns = df.columns.str.lower()`) is traced cross-file
  too — see [docs/usage.md's "Supported column-set transforms"](docs/usage.md#supported-column-set-transforms).

---

## Static Analysis

typedframes provides **two ways** to check your code:

### Option 1: Standalone Checker (Fast)

```shell
# Blazing fast Rust-based checker
typedframes check src/

# Output (ty-style, auto-colored in terminals):
# src/analysis.py:23:8: error[unknown-column] Column 'profit' not in SalesData
# src/pipeline.py:56:8: error[unknown-column] Column 'user_name' not in UserData
# ✗ Found 2 errors in 47 files (0.0s)
```

**Features:**
- Catches column name errors
- Validates schema mismatches between functions
- Validates function parameter contracts across files, including transitively through chains of
  helper functions (`missing-column`)
- Checks both pandas and polars code
- Significantly faster than mypy (see benchmarks below)

**Use this for:**
- Fast feedback during development
- CI/CD pipelines
- Pre-commit hooks

**Configuration:**
```shell
# Check specific files
typedframes check src/pipeline.py

# Check directory (builds cross-file index automatically)
typedframes check src/

# Fail on any error (for CI)
typedframes check src/ --strict

# JSON output
typedframes check src/ --output-format=json

# Skip cross-file index (single-file mode, faster for quick checks)
typedframes check src/ --no-index

# Suppress all warnings (untracked-dataframe, dropped-unknown-column)
typedframes check src/ --no-warnings

# Enforce minimum DataFrame schema coverage (see below)
typedframes check src/ --coverage-fail-under=90

# Show which DataFrames lack column info, per file
typedframes check src/ --coverage-detail=term-missing
```

To suppress warnings project-wide, add to `pyproject.toml`:
```toml
[tool.typedframes]
enabled = true
warnings = false
```

### Option 2: Mypy Plugin (Comprehensive)

```shell
# Add to pyproject.toml
[tool.mypy]
plugins = ["typedframes.mypy"]

# Or mypy.ini
[mypy]
plugins = typedframes.mypy

# Run mypy
mypy src/
```

**Features:**
- Full type checking across your codebase
- Catches column errors AND regular type errors
- IDE integration (VSCode, PyCharm)
- Works with existing mypy configuration

**Use this for:**
- Comprehensive type checking
- Integration with existing mypy setup
- IDE error highlighting

### Supported Operations

The checker tracks schema changes through `rename`, `drop`, `assign`, `select`, `pop`,
`insert`, `del`, subscript assignment, `merge`, and `concat`. Row-passthrough operations
like `filter`, `query`, `head`, `sort_values`, and `dropna` are validated without schema
changes. Operations with runtime-dependent output (`join`, `pivot`, `melt`, `groupby`,
`apply`, etc.) are left untracked to avoid false positives.

See the full [Method Matrix](https://typedframes.readthedocs.io/en/latest/method-matrix/)
for the complete list of tracked, passthrough, and untracked operations, plus the error
code reference.

### Pre-commit Hook

```yaml
# .pre-commit-config.yaml
repos:
  - repo: https://github.com/w-martin/typedframes
    rev: v0.5.1
    hooks:
      - id: typedframes
```

The hook defaults to `typedframes check . --strict` — one run per commit rather than one
per staged file, so the cross-file index still sees the modules the commit didn't touch.
`--strict` is what makes it block the commit: `typedframes check` exits 0 by default even
when it finds real errors.

`args` replaces that default list wholesale, so repeat the path and `--strict` when
narrowing the path or turning on the coverage gate:

```yaml
# .pre-commit-config.yaml — with coverage gating
repos:
  - repo: https://github.com/w-martin/typedframes
    rev: v0.5.1
    hooks:
      - id: typedframes
        args: [src/, --strict, --coverage-fail-under=90]
```

pre-commit builds that hook from this repository's source, which needs a Rust toolchain.
To install the prebuilt PyPI wheel instead, declare it as a local hook:

```yaml
repos:
  - repo: local
    hooks:
      - id: typedframes
        name: typedframes check
        entry: typedframes check . --strict
        language: python
        additional_dependencies: ["typedframes==0.5.0"]
        types_or: [python, jupyter]
        pass_filenames: false
```

### GitHub Actions

```yaml
# .github/workflows/typedframes.yml
name: typedframes
on: [push, pull_request]

jobs:
  typedframes:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: w-martin/typedframes@v0.5.1
        with:
          path: src/
          coverage-fail-under: "90"
```

The action installs the PyPI wheel into a throwaway virtualenv and runs the checker with
`--output-format=github`, so errors arrive as annotations on the pull request diff.

| Input | Default | |
|-------|---------|-|
| `path` | `.` | File or directory to check |
| `version` | `latest` | PyPI version to install, e.g. `"0.5.0"` |
| `strict` | `true` | Fail the step on errors — `typedframes check` exits 0 without it |
| `coverage-fail-under` | *(unset)* | Minimum DataFrame schema coverage, e.g. `"90"` |
| `coverage-detail` | `summary` | Or `term-missing` for the per-file breakdown |
| `no-warnings` | `false` | Suppress warning-level diagnostics |
| `args` | *(empty)* | Escape hatch for `--no-index`, `--lenient-ingest`, `--no-info` |

---

## DataFrame Schema Coverage Thresholds (Opt-In)

**DataFrame schema coverage** is the fraction of DataFrames `typedframes check` could
resolve column information for — the analogue of the "type coverage" reported by mypy,
pyright, and pyre, and unrelated to test coverage. That number is informational by
default. If you want it enforced — failing the run when too much of your code is
invisible to the checker — enable a threshold:

```toml
[tool.typedframes.coverage]
enabled = true
fail_under = 90.0
```

or as a one-off, without touching config:

```shell
typedframes check src/ --coverage-fail-under=90
```

This is **entirely opt-in** — with no `[tool.typedframes.coverage]` table and no
`--coverage-fail-under`, nothing changes: no threshold, no exit-code difference. Per-path
overrides (e.g. a lower bar for `legacy/**`), a `--coverage-detail=term-missing`
breakdown of exactly which DataFrames cost you coverage (as text or, combined with
`--output-format=json`, as structured JSON), and the full config reference all live in
the
[DataFrame schema coverage thresholds guide](docs/usage.md#dataframe-schema-coverage-thresholds).

## Static Analysis Performance

Fast feedback reduces development time. The typedframes Rust binary provides near-instant column checking.

**Benchmark results** (20 runs, 3 warmup, caches cleared between runs):
*2026-08-18 · Darwin 25.6.0 · arm · CPython 3.14.4 · 64GiB RAM · Great Expectations pinned @ 1.20.0*

| Tool | Version | What it does | typedframes (13 files) | great_expectations (485 files) |
|------|---------|--------------|------------------------|--------------------------------|
| typedframes | 0.4.1 | DataFrame column checker | 51ms ±918µs (IQR 1ms) | 219ms ±2ms (IQR 4ms) |
| ruff | 0.16.3 | Linter (no type checking) | 30ms ±764µs (IQR 922µs) | 233ms ±3ms (IQR 4ms) |
| ty | 0.0.72 | Type checker | 73ms ±1ms (IQR 2ms) | 810ms ±10ms (IQR 13ms) |
| pyrefly | 1.2.0 | Type checker | 104ms ±2ms (IQR 2ms) | 276ms ±9ms (IQR 14ms) |
| mypy | 2.3.1 | Type checker (no plugin) | 3.07s ±17ms (IQR 25ms) | 4.56s ±29ms (IQR 57ms) |
| mypy + typedframes | 2.3.1 | Type checker + column checker | 3.07s ±13ms (IQR 17ms) | 4.84s ±20ms (IQR 23ms) |
| pyright | 1.1.411 | Type checker | 781ms ±5ms (IQR 6ms) | 3.46s ±25ms (IQR 41ms) |

*Run `uv run python benchmarks/benchmark_checkers.py` to reproduce.*

The typedframes binary resolves column names within a file and, when a project index is present, across files too.
Run `typedframes check src/` to build the index automatically and catch errors like `df = load_users(); df["typo"]`
even when `load_users` is defined in another module. Pass `--no-index` to skip the index and check each file in
isolation. Full type checkers (mypy, pyright, ty) analyze all Python types across your entire codebase. Use both: the
binary for fast iteration, mypy for comprehensive checking.

The standalone checker is built with [`ruff_python_parser`](https://github.com/astral-sh/ruff) for Python AST
parsing.

**Note:** ty (Astral) does not currently support mypy plugins, so use the standalone binary for column checking with ty.

---

## Type Safety With Multiple Backends

typedframes uses **native backend types** to ensure complete type safety:

```python
from typing import Annotated
import pandas as pd
import polars as pl
from typedframes import BaseSchema, Column


class UserData(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)


# Pandas pipeline - type checker knows pandas methods
def pandas_analyze(df: Annotated[pd.DataFrame, UserData]) -> Annotated[pd.DataFrame, UserData]:
    return df[df["user_id"] > 100]  # ✓ Pandas syntax


# Polars pipeline - type checker knows polars methods
def polars_analyze(df: Annotated[pl.DataFrame, UserData]) -> Annotated[pl.DataFrame, UserData]:
    return df.filter(pl.col("user_id") > 100)  # ✓ Polars syntax


# Use native types throughout
df_pandas: Annotated[pd.DataFrame, UserData] = pd.read_csv("data.csv")
df_polars: Annotated[pl.DataFrame, UserData] = pl.read_csv("data.csv")

pandas_analyze(df_pandas)  # ✓ OK
polars_analyze(df_polars)  # ✓ OK
```

---

## Advanced Usage

### Merges, Joins, and Filters

Schema-typed DataFrames preserve their type through common operations:

**Pandas:**

```python
from typing import Annotated
import pandas as pd
from typedframes import BaseSchema, Column


class UserSchema(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)


class OrderSchema(BaseSchema):
    order_id = Column(type=int)
    user_id = Column(type=int)
    total = Column(type=float)


# Schema preserved through filtering
def get_active_users(df: Annotated[pd.DataFrame, UserSchema]) -> Annotated[pd.DataFrame, UserSchema]:
    return df[df["user_id"] > 100]  # ✓ Validated by checker


# Schema preserved through merges
users: Annotated[pd.DataFrame, UserSchema] = pd.read_csv("users.csv")
orders: Annotated[pd.DataFrame, OrderSchema] = pd.read_csv("orders.csv")
merged = users.merge(orders, on=UserSchema.user_id.s)
```

**Polars:**
```python
from typing import Annotated
import polars as pl


# Schema columns work in filter expressions
def filter_users(df: Annotated[pl.DataFrame, UserSchema]) -> pl.DataFrame:
    return df.filter(pl.col("user_id") > 100)


# Schema columns work in join expressions
def join_data(
    users: Annotated[pl.DataFrame, UserSchema],
    orders: Annotated[pl.DataFrame, OrderSchema],
) -> pl.DataFrame:
    return users.join(
        orders,
        left_on=UserSchema.user_id.s,
        right_on=OrderSchema.user_id.s,
    )


# Schema columns work in select expressions
def select_columns(df: Annotated[pl.DataFrame, UserSchema]) -> pl.DataFrame:
    return df.select([UserSchema.user_id.s, UserSchema.email.s])
```

### Dynamic Column Matching

Perfect for time-series data where column counts change. Regex ColumnSets document which columns belong
to a group and are validated by the static checker. The `.s` property gives you the list of column names
for explicit (non-regex) ColumnSets; for non-regex groups you can also use `.cols()` for polars expressions.

```python
from typing import Annotated
import pandas as pd
from typedframes import BaseSchema, Column, ColumnSet, ColumnGroup


class SensorReadings(BaseSchema):
    timestamp = Column(type=str)
    # Explicit sensor columns — refactor-safe list access via .s
    sensors = ColumnSet(type=float, members=["sensor_1", "sensor_2", "sensor_3"])


df: Annotated[pd.DataFrame, SensorReadings] = pd.read_csv("readings.csv")
df[SensorReadings.sensors.s].mean()  # ✓ Expands to df[["sensor_1", "sensor_2", "sensor_3"]].mean()
```

For logical grouping across multiple ColumnSets:

```python
class TimeSeriesData(BaseSchema):
    timestamp = Column(type=str)
    temperature = ColumnSet(type=float, members=["temp_1", "temp_2", "temp_3"])
    pressure = ColumnSet(type=float, members=["pressure_1", "pressure_2"])

    # Group for convenient access to all sensor columns
    sensors = ColumnGroup(members=[temperature, pressure])


df: Annotated[pd.DataFrame, TimeSeriesData] = pd.read_csv("sensors.csv")
avg_temp = df[TimeSeriesData.temperature.s].mean()
all_readings = df[TimeSeriesData.sensors.s].describe()
```

### Schema Composition

Compose upward — build bigger schemas from smaller ones via inheritance. Type checkers see all columns natively.

```python
from typing import Annotated
import pandas as pd
from typedframes import BaseSchema, Column


# Start with the smallest useful schema
class UserPublic(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)
    name = Column(type=str)


# Extend it — never strip down
class UserFull(UserPublic):
    password_hash = Column(type=str)


class Orders(BaseSchema):
    order_id = Column(type=int)
    user_id = Column(type=int)
    total = Column(type=float)


# Combine via multiple inheritance
class UserOrders(UserPublic, Orders):
    """Type checkers see all columns from both parents."""

    ...


# Or use the + operator
UserOrdersDynamic = UserPublic + Orders

users: Annotated[pd.DataFrame, UserPublic] = pd.read_csv("users.csv")
orders: Annotated[pd.DataFrame, Orders] = pd.read_csv("orders.csv")
merged: Annotated[pd.DataFrame, UserOrders] = users.merge(orders, on=UserPublic.user_id.s)
```

Overlapping columns with the same type are allowed (common after merges). Conflicting types raise `SchemaConflictError`.

See [`examples/features/schema_algebra_example.py`](examples/features/schema_algebra_example.py) for a complete walkthrough.

---

## Comparison

### Feature Matrix (Static Analysis Focus)

Comprehensive comparison of pandas/DataFrame typing and validation tools. **typedframes focuses on static analysis**
—catching errors at lint-time before your code runs.

| Feature                         | typedframes            | Pandera     | Great Expectations | strictly_typed_pandas | pandas-stubs | dataenforce | pandas-type-checks | StaticFrame      | narwhals | dataframely      | patito           |
|---------------------------------|------------------------|-------------|--------------------|-----------------------|--------------|-------------|--------------------|------------------|----------|------------------|------------------|
| **Version tested**              | 0.4.1                  | 0.32.1      | 1.20.0             | 0.3.7                 | 3.0.5        | 0.1.2       | 1.1.3              | 5.1.1            | 2.24.0   | 3.0.0            | 0.8.6            |
| **Analysis Type**               |
| When errors are caught          | **Static (lint-time)** | Runtime     | Runtime            | Runtime               | Static       | Runtime     | Runtime            | Runtime          | Runtime  | Runtime          | Runtime          |
| **Static Analysis (our focus)** |
| Mypy plugin                     | ✅ Yes                  | ⚠️ Limited  | ❌ No               | ❌ No                  | ✅ Yes        | ❌ No        | ❌ No               | ⚠️ Basic         | ❌ No     | ❌ No             | ❌ No             |
| Standalone checker              | ✅ Rust (ms-scale)      | ❌ No        | ❌ No               | ❌ No                  | ❌ No         | ❌ No        | ❌ No               | ❌ No             | ❌ No     | ❌ No             | ❌ No             |
| Column name checking            | ✅ Yes                  | ⚠️ Limited  | ❌ No               | ❌ No                  | ❌ No         | ❌ No        | ❌ No               | ❌ No             | ❌ No     | ❌ No             | ❌ No             |
| Column type checking            | ✅ Yes                  | ⚠️ Limited  | ❌ No               | ❌ No                  | ❌ No         | ❌ No        | ❌ No               | ❌ No             | ❌ No     | ❌ No             | ❌ No             |
| Typo suggestions                | ✅ Yes                  | ❌ No        | ❌ No               | ❌ No                  | ❌ No         | ❌ No        | ❌ No               | ❌ No             | ❌ No     | ❌ No             | ❌ No             |
| Coverage gate (`--coverage-fail-under`)  | ✅ Yes                  | ❌ No        | ❌ No               | ❌ No                  | ❌ No         | ❌ No        | ❌ No               | ❌ No             | ❌ No     | ❌ No             | ❌ No             |
| **Runtime Validation**          |
| Data validation                 | ❌ No                   | ✅ Excellent | ✅ Excellent        | ✅ typeguard           | ❌ No         | ✅ Yes       | ✅ Yes              | ✅ Yes            | ❌ No     | ✅ Yes            | ✅ Yes            |
| Value constraints               | ❌ No                   | ✅ Yes       | ✅ Excellent        | ❌ No                  | ❌ No         | ❌ No        | ❌ No               | ✅ Yes            | ❌ No     | ✅ Yes            | ✅ Yes            |
| **Schema Features**             |
| Column grouping                 | ✅ ColumnGroup          | ❌ No        | ❌ No               | ❌ No                  | ❌ No         | ❌ No        | ❌ No               | ❌ No             | ❌ No     | ❌ No             | ❌ No             |
| Regex column matching           | ✅ Yes                  | ❌ No        | ❌ No               | ❌ No                  | ❌ No         | ❌ No        | ❌ No               | ❌ No             | ❌ No     | ❌ No             | ❌ No             |
| **Backend Support**             |
| Pandas                          | ✅ Yes                  | ✅ Yes       | ✅ Yes              | ✅ Yes                 | ✅ Yes        | ✅ Yes       | ✅ Yes              | ❌ Own            | ✅ Yes    | ❌ No             | ⚠️ Limited        |
| Polars                          | ✅ Yes                  | ✅ Yes       | ❌ No               | ❌ No                  | ❌ No         | ❌ No        | ❌ No               | ❌ Own            | ✅ Yes    | ✅ Yes (only)     | ✅ Yes            |
| DuckDB, cuDF, etc.              | ❌ No                   | ❌ No        | ✅ Spark, SQL       | ❌ No                  | ❌ No         | ❌ No        | ❌ No               | ❌ No             | ✅ Yes    | ❌ No             | ❌ No             |
| **Project Status (Aug 2026)**   |
| Active development              | ✅ Yes                  | ✅ Yes       | ✅ Yes              | ⚠️ Low                | ✅ Yes        | ❌ Inactive  | ⚠️ Low             | ✅ Yes            | ✅ Yes    | ✅ Yes            | ✅ Yes            |

**Legend:** ✅ Full support | ⚠️ Limited/Partial | ❌ Not supported

### Tool Descriptions

- **[Pandera](https://pandera.readthedocs.io/)** (v0.32.1): Excellent runtime validation. Static analysis support exists
  but has limitations—column access via `df["column"]` is not validated, and schema mismatches between functions may not
  be caught.

- **[strictly_typed_pandas](https://strictly-typed-pandas.readthedocs.io/)** (v0.3.7): Provides `DataSet[Schema]` type
  hints for runtime validation via typeguard. Despite documentation implying mypy support, there is no mypy plugin —
  column access errors are not caught statically. No standalone checker. No polars support.

- **[pandas-stubs](https://github.com/pandas-dev/pandas-stubs)** (v3.0.5): Official pandas type stubs. Provides
  API-level types but no column-level checking.

- **[dataenforce](https://github.com/CedricFR/dataenforce)** (v0.1.2, the only release ever published): Runtime
  validation via decorator. Appears inactive/abandoned. Broken on every currently-supported Python version
  (3.11 through 3.14) due to removal of internal typing APIs (`typing._TypingEmpty`) it depends on — confirmed
  working only as far back as Python 3.9.

- **[pandas-type-checks](https://pypi.org/project/pandas-type-checks/)** (v1.1.3): Runtime validation decorator. No
  static analysis.

- **[StaticFrame](https://github.com/static-frame/static-frame)** (v5.1.1): Alternative immutable DataFrame library.
  Not compatible with pandas/polars — requires a full rewrite to StaticFrame's own API. Column access is still
  string-based; mypy does not catch column name typos. Type safety comes from immutability guarantees, not schema checking.

- **[narwhals](https://narwhals-dev.github.io/narwhals/)** (v2.24.0): Compatibility layer that provides a unified API
  across pandas, polars, DuckDB, cuDF, and more. Solves a different problem—write-once-run-anywhere portability, not
  type safety. See [Why Abstraction Layers Don't Solve Type Safety](#why-abstraction-layers-dont-solve-type-safety)
  below.

- **[Great Expectations](https://greatexpectations.io/)** (v1.20.0): Comprehensive data quality framework. Defines
  "expectations" (assertions) about data values, distributions, and schema properties. Excellent for runtime
  validation, data documentation, and data quality monitoring. No static analysis or column-level type checking in
  code. Supports pandas, Spark, and SQL backends.

- **[dataframely](https://github.com/Quantco/dataframely)** (v3.0.0): Polars-only runtime validation library from Quantco.
  Schemas are defined as classes inheriting `dy.Schema` with typed descriptor fields (`dy.String()`, `dy.Float64()`)
  and `@dy.rule()` decorators for cross-column and group-level constraints. Returns `dy.DataFrame[Schema]` generic
  types that give call-site narrowing to type checkers, but does not validate column subscript access inside function
  bodies, and (as of 3.0) that narrowing doesn't even survive a `.filter()` call — it returns a plain `pl.DataFrame`.
  3.0 also removed the `dy.Series` type entirely; column access now returns a plain `pl.Series`. No lint-time or
  static analysis capability. Supports nullability, string constraints, numeric bounds, cross-column rules, soft
  validation, test data generation, and SQLAlchemy/PyArrow export.

- **[patito](https://github.com/JakobGM/patito)** (v0.8.6): Runtime validation library using a Pydantic-style `patito.Model`
  class. Polars is the primary backend; pandas is supported but works by converting to Polars via PyArrow (an
  undeclared dependency). No static analysis or standalone checker.

### Type Checkers (Not DataFrame-Specific)

These are general Python type checkers. They don't validate DataFrame column names, but they can be used alongside
typedframes for comprehensive type checking:

- **[mypy](https://mypy-lang.org/)** (v2.3.1): The original Python type checker. typedframes provides a mypy plugin for
  column checking. See [performance benchmarks](#static-analysis-performance).

- **[ty](https://github.com/astral-sh/ty)** (v0.0.72, Astral): New Rust-based type checker, faster than mypy on
  large codebases. Does not support mypy plugins—use typedframes standalone checker.

- **[pyrefly](https://pyrefly.org/)** (v1.2.0, Meta): Rust-based type checker from Meta, replacement for Pyre. Fast,
  but no DataFrame column checking.

- **[pyright](https://github.com/microsoft/pyright)** (v1.1.411, Microsoft): Type checker powering Pylance/VSCode. No
  mypy plugin support—use typedframes standalone checker.

### Not Directly Comparable

These tools serve different purposes:

- **[pandas_lint](https://github.com/Jean-EstevezT/pandas_lint)**: Lints pandas code patterns (performance, best
  practices). Does not check column names/types.
- **[pandas-vet](https://github.com/deppen8/pandas-vet)**: Flake8 plugin for pandas best practices. Does not check
  column names/types.

### When to Use What

| Use Case                                             | Recommended Tool                    |
|------------------------------------------------------|-------------------------------------|
| Static column checking (existing pandas/polars)      | **typedframes**                     |
| Runtime data validation                              | Pandera                             |
| Both static + runtime                                | typedframes + `to_pandera_schema()` |
| Cross-library portability (write once, run anywhere) | narwhals                            |
| Data quality monitoring / pipeline validation        | Great Expectations                  |
| Immutable DataFrames from scratch                    | StaticFrame                         |
| Pandas API type hints only                           | pandas-stubs                        |

---

## Pandera Integration

Convert typedframes schemas to [Pandera](https://pandera.readthedocs.io/) schemas for runtime validation. Define your
schema once, get both static and runtime checking.

```shell
pip install typedframes[pandera]
```

```python
from typedframes import BaseSchema, Column
from typedframes.pandera import to_pandera_schema
import pandas as pd


class UserData(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)
    age = Column(type=int, nullable=True)


# Convert to pandera schema
pandera_schema = to_pandera_schema(UserData)

# Validate data at runtime
df = pd.read_csv("users.csv")
validated_df = pandera_schema.validate(df)  # Raises SchemaError on failure
```

The conversion maps:

- `Column` type/nullable/alias to `pa.Column` dtype/nullable/name
- `ColumnSet` with explicit members to individual `pa.Column` entries
- `ColumnSet` with regex to `pa.Column(regex=True)`
- `allow_extra_columns` to pandera's `strict` mode

---

## Examples

Runnable versions of everything shown in [Quick Start](#quick-start) and
[Advanced Usage](#advanced-usage) live under [`examples/features/`](examples/features/):

- [`annotated_pandas_example.py`](examples/features/annotated_pandas_example.py) —
  `Annotated[pd.DataFrame, Schema]` with string and `.s` descriptor column access
- [`annotated_polars_example.py`](examples/features/annotated_polars_example.py) —
  the same, with `pl.col()` and `.col` descriptor expressions
- [`typedframes_example.py`](examples/features/typedframes_example.py) — pandas and
  polars side by side against one shared schema
- [`schema_algebra_example.py`](examples/features/schema_algebra_example.py) —
  composing schemas via inheritance and the `+` operator
- [`inference_example.py`](examples/features/inference_example.py) — all four
  column-inference modes with annotated ✓/✗ comments, no `BaseSchema` at all
- [`multi_file_inference/`](examples/features/multi_file_inference/) and
  [`multi_file_with_schema/`](examples/features/multi_file_with_schema/) — the same
  cross-file pipeline checked with and without schemas, see
  [`examples/features/README.md`](examples/features/README.md) for what each one catches

---

## Philosophy

### Type Safety Over Validation

We believe static analysis catches bugs earlier and cheaper than runtime validation.

**typedframes focuses on:**
- ✅ Catching errors at lint-time
- ✅ Zero runtime overhead
- ✅ Developer experience

**We explicitly don't focus on:**
- ❌ Runtime data validation (use Pandera)
- ❌ Statistical checks (use Pandera)
- ❌ Data quality monitoring (use Great Expectations)

**Important:** An `Annotated[pd.DataFrame, Schema]` type annotation is a *trust assertion*, not a validation step.
It tells the type checker "this DataFrame conforms to this schema" without verifying the actual data. The linter catches
mistakes in your code (wrong column names, schema mismatches between functions), but it cannot verify that a CSV file
contains the expected columns. For runtime validation of external data, use
[`to_pandera_schema()`](#pandera-integration) to convert your typedframes schemas to Pandera schemas.

### Native Backend Types

We use native `Annotated[pd.DataFrame, Schema]` and `Annotated[pl.DataFrame, Schema]` types because pandas and
polars have fundamentally different APIs. By annotating native objects rather than wrapping them in custom classes,
typedframes lets you use each library's full, native API while still getting schema-level type safety.

**Trade-offs we avoid:**
- ❌ Custom wrapper classes (you lose IDE completion for native methods)
- ❌ "Universal DataFrame" abstractions (you lose library-specific features)
- ❌ Lowest-common-denominator APIs

### Why Abstraction Layers Don't Solve Type Safety

Tools like [narwhals](https://narwhals-dev.github.io/narwhals/) solve a different problem: writing portable code that runs on pandas, polars, DuckDB, cuDF, and other backends. This is useful for library authors who want to support multiple backends without maintaining separate codebases.

However, abstraction layers don't provide column-level type safety:

```python
import narwhals as nw


def process(df: nw.DataFrame) -> nw.DataFrame:
    # No static checking - "revenue" typo won't be caught until runtime
    return df.filter(nw.col("revnue") > 100)  # Typo: "revnue" vs "revenue"
```

**The fundamental issue:** Abstraction layers abstract over *which library* you're using, not *what columns* your data has. They can't know at lint-time whether "revenue" is a valid column in your DataFrame.

typedframes solves the orthogonal problem of schema safety:

```python
from typing import Annotated
import polars as pl
from typedframes import BaseSchema, Column


class SalesData(BaseSchema):
    revenue = Column(type=float)


def process(df: Annotated[pl.DataFrame, SalesData]) -> pl.DataFrame:
    return df.filter(pl.col("revnue") > 100)  # ✗ Error at lint-time: 'revnue' not in SalesData
```

**Use narwhals when:** You're writing a library that needs to work with multiple DataFrame backends.

**Use typedframes when:** You want to catch column name/type errors before your code runs.

### Why No Built-in Validation?

Ideally, validation happens at the point of data ingestion rather than in Python application code. If you're validating
DataFrames in Python, consider whether your data pipeline could enforce constraints earlier. Use Pandera for cases where
runtime validation is genuinely necessary.

---

## License

MIT License - see [LICENSE](LICENSE)

---

## FAQ

**Q: Do I need to choose between pandas and polars?**
A: No. Define your schema once, use it with both. Just use `Annotated[pd.DataFrame, Schema]` or `Annotated[pl.DataFrame, Schema]` in your function signatures.

**Q: Does this replace Pandera?**
A: No, it complements it. Use typedframes for static analysis, and `to_pandera_schema()` to convert your schemas to
Pandera for runtime validation. See [Pandera Integration](#pandera-integration).

**Q: Is the standalone checker required?**
A: No. You can use just the mypy plugin, just the standalone checker, or both. They catch the same errors.

**Q: What works without any plugin?**
A: Any type checker (mypy, pyright, ty) understands `Annotated[pd.DataFrame, Schema]` as a plain `pd.DataFrame` —
no plugin or stubs needed for basic type checking. Column *name* validation (catching typos like `df["revnue"]` in
string-based access) still requires the standalone checker or mypy plugin.

**Q: What about pyright/pylance users?**
A: The mypy plugin doesn't work with pyright. Use the standalone checker (`typedframes check`) for column name
validation. Schema descriptor access (`df[Schema.column]`) works natively in pyright without any plugin.

**Q: Do I need to write `BaseSchema` classes to get value?**
A: No. The standalone checker works entirely from inference: `usecols=`/`columns=`/`dtype=` arguments on read
calls give it enough information to validate column access and propagate that knowledge through method chains
(`rename`, `drop`, `assign`, `select`, …). `BaseSchema` is a progressive enhancement that unlocks cross-file
awareness (schemas travel with function return types across module boundaries) and IDE autocomplete via
descriptors — but the checker catches real column errors from day one without it. See
[`examples/features/multi_file_inference/`](examples/features/multi_file_inference/) for a complete demo with no schema classes.

**Q: Does this work with existing pandas/polars code?**
A: Yes. You can gradually adopt typedframes by adding schemas to new code. Existing code continues to work.
Start by adding `usecols=` to your read calls to get immediate column validation, then add `BaseSchema`
classes incrementally where cross-file tracking or autocomplete is most valuable.

**Q: What if my column name conflicts with a pandas/polars method?**
A: No problem. Since column access uses bracket syntax with schema descriptors (`df[Schema.mean]`), there is no conflict
with DataFrame methods (`df.mean()`). Both work independently.

---

## Credits

Built by developers who believe DataFrame bugs should be caught at lint-time, not in production.

Inspired by the needs of ML/data science teams working with complex data pipelines.

---

**Questions? Issues? Ideas?** [Open an issue](https://github.com/w-martin/typedframes/issues)

**Ready to catch DataFrame bugs before runtime?** `pip install typedframes`

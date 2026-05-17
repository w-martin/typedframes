# typedframes

[![CI](https://github.com/w-martin/typedframes/actions/workflows/publish.yml/badge.svg)](https://github.com/w-martin/typedframes/actions/workflows/publish.yml)
[![PyPI version](https://img.shields.io/pypi/v/typedframes.svg)](https://pypi.org/project/typedframes/)
[![Python versions](https://img.shields.io/pypi/pyversions/typedframes.svg)](https://pypi.org/project/typedframes/)
[![Coverage](https://codecov.io/gh/w-martin/typedframes/branch/main/graph/badge.svg)](https://codecov.io/gh/w-martin/typedframes)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

> ⚠️ **Project Status: Proof of Concept**
>
> `typedframes` (v0.2.1) is currently an experimental proof-of-concept. The core static analysis and mypy/Rust
> integrations work, but expect rough edges. The codebase prioritizes demonstrating the viability of static DataFrame
> schema validation over production-grade stability.
>
**Static analysis for pandas and polars DataFrames. Catch column errors at lint-time, not runtime.**

```python
from typing import Annotated
import pandas as pd
from typedframes import BaseSchema, Column


class UserData(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)
    signup_date = Column(type=str)


df: Annotated[pd.DataFrame, UserData] = pd.read_csv("users.csv")
df['user_id']    # ✓ Validated by checker
df['username']   # ✗ unknown-column: Column 'username' not in UserData
```

**Descriptors as a bridge:** define once in `Column(type=int)`, access as `df[UserData.user_id.s]` (pandas string
access) or `df.select(UserData.revenue.col)` (polars expression). Refactor by changing the descriptor definition —
all `.s` and `.col` references update automatically. No find-and-replace across string literals.

---

## Table of Contents

- [Why typedframes?](#why-typedframes)
- [Installation](#installation)
- [Quick Start](#quick-start)
- [Column Inference](#column-inference)
- [Static Analysis](#static-analysis)
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

**The problem:** Many pandas bugs are column mismatches. You access a column that doesn't exist, pass the wrong schema to a function, or make a typo. These errors only surface at runtime, often in production.

**The solution:** Define your DataFrame schemas as Python classes. Get static type checking that catches column errors before you even run your code.

**What you get:**

- ✅ **Works without schema annotations** - Column inference from `usecols=`, `dtype=`, and method chains catches errors on unannotated code
- ✅ **Cross-file awareness** - Add `BaseSchema` and typed return annotations to follow schemas across module boundaries
- ✅ **Static analysis** - Catch column errors at lint-time with mypy or the standalone checker
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

The Rust-based checker is included — no separate install needed.

---

## Quick Start

### Run on existing code

The checker works from day one without any schema classes. Pass `usecols=` / `columns=` to your read calls and
column access is validated automatically — no schema classes needed:

```python
import pandas as pd

# Checker infers {order_id, amount, status} from usecols=
orders = pd.read_csv("orders.csv", usecols=["order_id", "amount", "status"])
print(orders["amount"])   # ✓ OK
print(orders["revenue"])  # ✗ unknown-column — 'revenue' not in inferred set
```

```shell
typedframes check src/
# src/pipeline.py:7:8: error[unknown-column] Column 'revenue' not in inferred set {order_id, amount, status}
# ✗ Found 1 error in 12 files (0.0s)
```

See [`examples/multi_file_inference/`](examples/multi_file_inference/) for a multi-file example with no `BaseSchema`
classes at all.

### Define Your Schema (Once)

Add `BaseSchema` classes when you want cross-file awareness and IDE autocomplete. Schemas travel with function return
types across module boundaries — the checker validates call sites even in files that have no `usecols=` of their own:

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
print(df['revenue'].sum())
print(df['profit'])  # ✗ unknown-column: Column 'profit' not in SalesData

# .s gives a refactor-safe string name from the descriptor
print(df[SalesData.revenue.s].sum())   # same as df['revenue'].sum()


# Type-safe function signature
def analyze(data: Annotated[pd.DataFrame, SalesData]) -> float:
    data['revenue']  # ✓ Validated by checker
    data['profit']   # ✗ unknown-column: 'profit' not in SalesData
    return data[SalesData.revenue.s].mean()
```

### Use With Polars

```python
from typing import Annotated
import polars as pl

# Annotate your variable — checker validates pl.col() references too
df: Annotated[pl.DataFrame, SalesData] = pl.read_csv("sales.csv")

# pl.col() references are now validated by the standalone checker
print(df.filter(pl.col('revenue') > 1000))
print(df.select(pl.col('profit')))  # ✗ unknown-column: Column 'profit' not in SalesData

# .col gives a refactor-safe polars expression from the descriptor
filtered = df.filter(SalesData.revenue.col > 1000)
grouped = df.group_by('customer_id').agg(SalesData.revenue.col.sum())
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
print(df["user_id"])   # ✓ OK — in usecols
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

**untracked-dataframe — unannotated data ingestion (off by default)**

By default, typedframes supports permissive Exploratory Data Analysis (EDA). When a DataFrame is loaded
via `pd.read_csv()` without `usecols=` or a schema annotation, the checker assumes an *Unknown* state
and bypasses strict column validation to avoid nagging you during discovery.

To lock down production CI/CD pipelines, opt in to `untracked-dataframe` warnings with `--strict-ingest`:

```shell
typedframes check src/ --strict-ingest
```

With strict ingestion enabled, loading a DataFrame without a schema or `usecols=` produces:

```python
df = pd.read_csv("users.csv")
# ⚠ untracked-dataframe: columns unknown at lint time; specify `usecols`/`columns` or
#   annotate: `df: Annotated[pd.DataFrame, MySchema] = pd.read_csv(...)`
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

### See Also

- [`examples/inference_example.py`](examples/inference_example.py) — single-file walkthrough of all four inference
  scenarios with annotated ✓/✗ comments.
- [`examples/multi_file_inference/`](examples/multi_file_inference/) — multi-file project checked with
  `typedframes check examples/multi_file_inference/`; no `BaseSchema` anywhere.
- [`examples/multi_file_with_schema/`](examples/multi_file_with_schema/) — same scenario with `BaseSchema`
  classes; the checker follows schemas across module boundaries via the project index.

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
- Checks both pandas and polars code
- 10-100x faster than mypy

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
typedframes check src/ --json

# Skip cross-file index (single-file mode, faster for quick checks)
typedframes check src/ --no-index

# Suppress all warnings (untracked-dataframe, dropped-unknown-column)
typedframes check src/ --no-warnings
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

---

## Static Analysis Performance

Fast feedback reduces development time. The typedframes Rust binary provides near-instant column checking.

**Benchmark results** (10 runs, 3 warmup, caches cleared between runs):

| Tool               | Version | What it does                  | typedframes (13 files) | great_expectations (490 files)† |
|--------------------|---------|-------------------------------|------------------------|---------------------------------|
| typedframes        | 0.2.0   | DataFrame column checker      | 9ms ±2ms               | 930µs ±89µs                     |
| ruff               | 0.15.4  | Linter (no type checking)     | 64ms ±16ms             | 360ms ±18ms                     |
| ty                 | 0.0.19  | Type checker                  | 115ms ±22ms            | 1.65s ±26ms                     |
| pyrefly            | 0.54.0  | Type checker                  | 3.78s ±7.53s           | 693ms ±33ms                     |
| mypy               | 1.19.1  | Type checker (no plugin)      | 13.85s ±1.08s          | 12.13s ±400ms                   |
| mypy + typedframes | 1.19.1  | Type checker + column checker | 13.51s ±273ms          | 13.89s ±491ms                   |
| pyright            | 1.1.408 | Type checker                  | 2.10s ±422ms           | 8.37s ±253ms                    |

*† great_expectations column from previous benchmark run.*

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
    return df[df['user_id'] > 100]  # ✓ Pandas syntax


# Polars pipeline - type checker knows polars methods
def polars_analyze(df: Annotated[pl.DataFrame, UserData]) -> Annotated[pl.DataFrame, UserData]:
    return df.filter(pl.col('user_id') > 100)  # ✓ Polars syntax


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
    return df[df['user_id'] > 100]  # ✓ Validated by checker


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
    return df.filter(pl.col('user_id') > 100)


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
df[SensorReadings.sensors.s].mean()    # ✓ Expands to df[["sensor_1", "sensor_2", "sensor_3"]].mean()
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

See [`examples/schema_algebra_example.py`](examples/schema_algebra_example.py) for a complete walkthrough.

---

## Comparison

### Feature Matrix (Static Analysis Focus)

Comprehensive comparison of pandas/DataFrame typing and validation tools. **typedframes focuses on static analysis**
—catching errors at lint-time before your code runs.

| Feature                         | typedframes            | Pandera     | Great Expectations | strictly_typed_pandas | pandas-stubs | dataenforce | pandas-type-checks | StaticFrame      | narwhals | dataframely      | patito           |
|---------------------------------|------------------------|-------------|--------------------|-----------------------|--------------|-------------|--------------------|------------------|----------|------------------|------------------|
| **Version tested**              | 0.2.0                  | 0.29.0      | 1.4.3              | 0.3.6                 | 3.0.0        | 0.1.2       | 1.1.3              | 3.7.0            | 2.16.0   | —                | —                |
| **Analysis Type**               |
| When errors are caught          | **Static (lint-time)** | Runtime     | Runtime            | Static + Runtime      | Static       | Runtime     | Runtime            | Static + Runtime | Runtime  | Runtime          | Runtime          |
| **Static Analysis (our focus)** |
| Mypy plugin                     | ✅ Yes                  | ⚠️ Limited  | ❌ No               | ✅ Yes                 | ✅ Yes        | ❌ No        | ❌ No               | ⚠️ Basic         | ❌ No     | ❌ No             | ❌ No             |
| Standalone checker              | ✅ Rust (~1ms)          | ❌ No        | ❌ No               | ❌ No                  | ❌ No         | ❌ No        | ❌ No               | ❌ No             | ❌ No     | ❌ No             | ❌ No             |
| Column name checking            | ✅ Yes                  | ⚠️ Limited  | ❌ No               | ✅ Yes                 | ❌ No         | ❌ No        | ❌ No               | ✅ Yes            | ❌ No     | ❌ No             | ❌ No             |
| Column type checking            | ✅ Yes                  | ⚠️ Limited  | ❌ No               | ✅ Yes                 | ❌ No         | ❌ No        | ❌ No               | ✅ Yes            | ❌ No     | ❌ No             | ❌ No             |
| Typo suggestions                | ✅ Yes                  | ❌ No        | ❌ No               | ❌ No                  | ❌ No         | ❌ No        | ❌ No               | ❌ No             | ❌ No     | ❌ No             | ❌ No             |
| **Runtime Validation**          |
| Data validation                 | ❌ No                   | ✅ Excellent | ✅ Excellent        | ✅ typeguard           | ❌ No         | ✅ Yes       | ✅ Yes              | ✅ Yes            | ❌ No     | ✅ Yes            | ✅ Yes            |
| Value constraints               | ❌ No                   | ✅ Yes       | ✅ Excellent        | ❌ No                  | ❌ No         | ❌ No        | ❌ No               | ✅ Yes            | ❌ No     | ✅ Yes            | ✅ Yes            |
| **Schema Features**             |
| Column grouping                 | ✅ ColumnGroup          | ❌ No        | ❌ No               | ❌ No                  | ❌ No         | ❌ No        | ❌ No               | ❌ No             | ❌ No     | ❌ No             | ❌ No             |
| Regex column matching           | ✅ Yes                  | ❌ No        | ❌ No               | ❌ No                  | ❌ No         | ❌ No        | ❌ No               | ❌ No             | ❌ No     | ❌ No             | ❌ No             |
| **Backend Support**             |
| Pandas                          | ✅ Yes                  | ✅ Yes       | ✅ Yes              | ✅ Yes                 | ✅ Yes        | ✅ Yes       | ✅ Yes              | ❌ Own            | ✅ Yes    | ❌ No             | ❌ No             |
| Polars                          | ✅ Yes                  | ✅ Yes       | ❌ No               | ❌ No                  | ❌ No         | ❌ No        | ❌ No               | ❌ Own            | ✅ Yes    | ✅ Yes (only)     | ✅ Yes (only)     |
| DuckDB, cuDF, etc.              | ❌ No                   | ❌ No        | ✅ Spark, SQL       | ❌ No                  | ❌ No         | ❌ No        | ❌ No               | ❌ No             | ✅ Yes    | ❌ No             | ❌ No             |
| **Project Status (Feb 2026)**   |
| Active development              | ✅ Yes                  | ✅ Yes       | ✅ Yes              | ⚠️ Low                | ✅ Yes        | ❌ Inactive  | ⚠️ Low             | ✅ Yes            | ✅ Yes    | ✅ Yes            | ✅ Yes            |

**Legend:** ✅ Full support | ⚠️ Limited/Partial | ❌ Not supported

### Tool Descriptions

- **[Pandera](https://pandera.readthedocs.io/)** (v0.29.0): Excellent runtime validation. Static analysis support exists
  but has limitations—column access via `df["column"]` is not validated, and schema mismatches between functions may not
  be caught.

- **[strictly_typed_pandas](https://strictly-typed-pandas.readthedocs.io/)** (v0.3.6): Provides `DataSet[Schema]` type
  hints with mypy support. No standalone checker. No polars support. Runtime validation via typeguard.

- **[pandas-stubs](https://github.com/pandas-dev/pandas-stubs)** (v3.0.0): Official pandas type stubs. Provides
  API-level types but no column-level checking.

- **[dataenforce](https://github.com/CedricFR/dataenforce)** (v0.1.2): Runtime validation via decorator. Marked as
  experimental/not production-ready. Appears inactive.

- **[pandas-type-checks](https://pypi.org/project/pandas-type-checks/)** (v1.1.3): Runtime validation decorator. No
  static analysis.

- **[StaticFrame](https://github.com/static-frame/static-frame)** (v3.7.0): Alternative immutable DataFrame library with
  built-in static typing. Not compatible with pandas/polars—requires using StaticFrame's own DataFrame implementation.

- **[narwhals](https://narwhals-dev.github.io/narwhals/)** (v2.16.0): Compatibility layer that provides a unified API
  across pandas, polars, DuckDB, cuDF, and more. Solves a different problem—write-once-run-anywhere portability, not
  type safety. See [Why Abstraction Layers Don't Solve Type Safety](#why-abstraction-layers-dont-solve-type-safety)
  below.

- **[Great Expectations](https://greatexpectations.io/)** (v1.4.3): Comprehensive data quality framework. Defines
  "expectations" (assertions) about data values, distributions, and schema properties. Excellent for runtime
  validation, data documentation, and data quality monitoring. No static analysis or column-level type checking in
  code. Supports pandas, Spark, and SQL backends.

- **[dataframely](https://github.com/Quantco/dataframely)**: Polars-only runtime validation library from Quantco.
  Schemas are defined as classes inheriting `dy.Schema` with typed descriptor fields (`dy.String()`, `dy.Float64()`)
  and `@dy.rule()` decorators for cross-column and group-level constraints. Returns `dy.DataFrame[Schema]` generic
  types that give call-site narrowing to type checkers, but does not validate column subscript access inside function
  bodies. No lint-time or static analysis capability. Supports nullability, string constraints, numeric bounds,
  cross-column rules, soft validation, test data generation, and SQLAlchemy/PyArrow export.

- **[patito](https://github.com/JakobGM/patito)**: Polars-only runtime validation library using a Pydantic-style
  `patito.Model` class. Validates DataFrames against model definitions using polars' native type system. No static
  analysis or standalone checker. Actively maintained (626 stars, last commit May 2026).

### Type Checkers (Not DataFrame-Specific)

These are general Python type checkers. They don't validate DataFrame column names, but they can be used alongside
typedframes for comprehensive type checking:

- **[mypy](https://mypy-lang.org/)** (v1.19.1): The original Python type checker. typedframes provides a mypy plugin for
  column checking. See [performance benchmarks](#static-analysis-performance).

- **[ty](https://github.com/astral-sh/ty)** (v0.0.19, Astral): New Rust-based type checker, 10-60x faster than mypy on
  large codebases. Does not support mypy plugins—use typedframes standalone checker.

- **[pyrefly](https://pyrefly.org/)** (v0.54.0, Meta): Rust-based type checker from Meta, replacement for Pyre. Fast,
  but no DataFrame column checking.

- **[pyright](https://github.com/microsoft/pyright)** (v1.1.408, Microsoft): Type checker powering Pylance/VSCode. No
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

### Basic CSV Processing

```python
from typing import Annotated
import pandas as pd
from typedframes import BaseSchema, Column


class Orders(BaseSchema):
    order_id = Column(type=int)
    customer_id = Column(type=int)
    total = Column(type=float)
    date = Column(type=str)


def calculate_revenue(orders: Annotated[pd.DataFrame, Orders]) -> float:
    return orders['total'].sum()


df: Annotated[pd.DataFrame, Orders] = pd.read_csv("orders.csv")
revenue = calculate_revenue(df)
```

### Time Series Analysis

```python
from typing import Annotated
import pandas as pd
from typedframes import BaseSchema, Column, ColumnSet, ColumnGroup


class SensorData(BaseSchema):
    timestamp = Column(type=str)
    temperature = ColumnSet(type=float, members=["temp_1", "temp_2", "temp_3"])
    humidity = ColumnSet(type=float, members=["humidity_1", "humidity_2"])

    all_sensors = ColumnGroup(members=[temperature, humidity])


df: Annotated[pd.DataFrame, SensorData] = pd.read_csv("sensors.csv")

# Clean, type-safe operations using .s for column name lists
avg_temp_per_row = df[SensorData.temperature.s].mean(axis=1)
all_readings_stats = df[SensorData.all_sensors.s].describe()
```

### Multi-Step Pipeline

```python
from typing import Annotated
import pandas as pd
from typedframes import BaseSchema, Column


class RawSales(BaseSchema):
    date = Column(type=str)
    product_id = Column(type=int)
    quantity = Column(type=int)
    price = Column(type=float)


class AggregatedSales(BaseSchema):
    date = Column(type=str)
    total_revenue = Column(type=float)
    total_quantity = Column(type=int)


def aggregate_daily(df: Annotated[pd.DataFrame, RawSales]) -> Annotated[pd.DataFrame, AggregatedSales]:
    result = df.groupby(RawSales.date.s).agg({
        RawSales.price.s: 'sum',
        RawSales.quantity.s: 'sum',
    }).reset_index()
    result.columns = pd.Index(['date', 'total_revenue', 'total_quantity'])
    return result  # type: ignore[return-value]


# Type-safe pipeline
raw: Annotated[pd.DataFrame, RawSales] = pd.read_csv("sales.csv")
aggregated = aggregate_daily(raw)


# Type checker validates schema transformations
def analyze(df: Annotated[pd.DataFrame, AggregatedSales]) -> float:
    df['total_revenue']  # ✓ OK
    df['price']          # ✗ Error: 'price' not in AggregatedSales
    return df[AggregatedSales.total_revenue.s].mean()
```

### Polars Performance Pipeline

```python
from typing import Annotated
import polars as pl
from typedframes import BaseSchema, Column


class LargeDataset(BaseSchema):
    id = Column(type=int)
    value = Column(type=float)
    category = Column(type=str)


def efficient_aggregation(df: Annotated[pl.DataFrame, LargeDataset]) -> pl.DataFrame:
    return (
        df.filter(pl.col('value') > 100)
        .group_by('category')
        .agg(pl.col('value').mean())
    )


# Polars handles large files efficiently
df: Annotated[pl.DataFrame, LargeDataset] = pl.read_csv("huge_file.csv")
result = efficient_aggregation(df)
```

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
    return df.filter(pl.col('revnue') > 100)  # ✗ Error at lint-time: 'revnue' not in SalesData
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

## Roadmap

**Shipped:**
- [x] Schema definition API
- [x] Pandas support
- [x] Polars support
- [x] Mypy plugin
- [x] Standalone checker (Rust)
- [x] Explicit backend types
- [x] Merge/join schema preservation
- [x] Schema Composition (multiple inheritance, `SchemaA + SchemaB`)
- [x] Column name collision warnings
- [x] Pandera integration (`to_pandera_schema()`)
- [x] Cross-file schema inference (project-level index, `--no-index` flag)
- [x] Aggressive column inference (untracked-dataframe/dropped-unknown-column warnings, method chain propagation)

**Planned:**

- [ ] **Opt-in data loading constraints** - `Field` class with constraints (`gt`, `ge`, `lt`, `le`), strictly isolated
  to `from_schema()` ingestion boundaries

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
[`examples/multi_file_inference/`](examples/multi_file_inference/) for a complete demo with no schema classes.

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

**Questions? Issues? Ideas?** [Open an issue](https://github.com/yourusername/typedframes/issues)

**Ready to catch DataFrame bugs before runtime?** `pip install typedframes`

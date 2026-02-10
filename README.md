# typedframes

**Static analysis for pandas and polars DataFrames. Catch column errors at lint-time, not runtime.**

```python
from typedframes import BaseSchema, Column
from typedframes.pandas import PandasFrame


class UserData(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)
    signup_date = Column(type=str)


def process(df: PandasFrame[UserData]) -> None:
    df['user_id']  # ✓ OK
    df['username']  # ✗ Error: Column 'username' not in UserData
```

---

## Table of Contents

- [Why typedframes?](#why-typedframes)
- [Installation](#installation)
- [Quick Start](#quick-start)
- [Static Analysis](#static-analysis)
- [Static Analysis Performance](#static-analysis-performance)
- [Comparison](#comparison)
- [Type Safety With Multiple Backends](#type-safety-with-multiple-backends)
- [Features](#features)
- [Advanced Usage](#advanced-usage)
- [Examples](#examples)
- [Philosophy](#philosophy)
- [FAQ](#faq)

---

## Why typedframes?

**The problem:** Many pandas bugs are column mismatches. You access a column that doesn't exist, pass the wrong schema to a function, or make a typo. These errors only surface at runtime, often in production.

**The solution:** Define your DataFrame schemas as Python classes. Get static type checking that catches column errors before you even run your code.

**What you get:**

- ✅ **Static analysis** - Catch column errors at lint-time with mypy or the standalone checker
- ✅ **Beautiful runtime UX** - `df.column_group.mean()` (pandas) instead of ugly column lists
- ✅ **Works with pandas AND polars** - Same schema API, explicit backend types
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

---

## Quick Start

### Define Your Schema (Once)

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
import pandas as pd
from typedframes.pandas import PandasFrame

# Load data with schema
df = PandasFrame.from_schema(pd.read_csv("sales.csv"), SalesData)

# Clean attribute access
print(df.revenue.sum())
print(df.metrics.mean())  # All metric_* columns


# Type-safe pandas operations
def analyze(data: PandasFrame[SalesData]) -> float:
    return data['revenue'].mean()  # ✓ Type checker knows this column exists
    # return data['profit'].mean()   # ✗ Error at lint-time


# Pandas methods work as expected
filtered = df[df['revenue'] > 1000]
grouped = df.groupby('customer_id')['revenue'].sum()
```

### Use With Polars

```python
from typedframes.polars import PolarsFrame
import polars as pl

# Same schema, different backend
df: PolarsFrame[SalesData] = pl.read_csv("sales.csv")

# Use schema column references for type-safe expressions
print(df.select(SalesData.revenue.col).sum())


# Type-safe polars operations
def analyze_polars(data: PolarsFrame[SalesData]) -> float:
    return data.select(SalesData.revenue.col).mean()
    # return data.select(['profit'])  # ✗ Error at lint-time


# Polars methods work as expected
filtered = df.filter(SalesData.revenue.col > 1000)
grouped = df.group_by('customer_id').agg(SalesData.revenue.col.sum())
```

---

## Static Analysis

typedframes provides **two ways** to check your code:

### Option 1: Standalone Checker (Fast)

```shell
# Blazing fast Rust-based checker
typedframes check src/

# Output:
# ✓ Checked 47 files in 0.2s
# ✗ src/analysis.py:23 - Column 'profit' not in SalesData
# ✗ src/pipeline.py:56 - Column 'user_name' not in UserData
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

# Check directory
typedframes check src/

# Fail on any error (for CI)
typedframes check src/ --strict

# JSON output
typedframes check src/ --json
```

### Option 2: Mypy Plugin (Comprehensive)

```shell
# Add to pyproject.toml
[tool.mypy]
plugins = ["typedframes_lint.mypy"]

# Or mypy.ini
[mypy]
plugins = typedframes_lint.mypy

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

---

## Static Analysis Performance

Fast feedback reduces development time. The typedframes Rust binary provides near-instant column checking.

**Benchmark results** (12 Python files, 5 runs each, caches cleared):

| Tool               | Version | What it does                  | Time         |
|--------------------|---------|-------------------------------|--------------|
| typedframes        | 0.1.0   | DataFrame column checker      | 1ms ±19µs    |
| ruff               | 0.14.13 | Linter (no type checking)     | 53ms ±9ms    |
| ty                 | 0.0.14  | Type checker                  | 131ms ±14ms  |
| pyrefly            | 0.51.1  | Type checker                  | 248ms ±14ms  |
| mypy               | 1.19.1  | Type checker (no plugin)      | 6.88s ±170ms |
| mypy + typedframes | 1.19.1  | Type checker + column checker | 6.98s ±92ms  |
| pyright            | 1.1.408 | Type checker                  | 2.02s ±201ms |

**Note:** On small codebases, startup time dominates. On larger projects, ty and pyrefly are typically 10-60x faster
than mypy/pyright.

*Run `uv run python benchmarks/benchmark_checkers.py` to reproduce.*

The typedframes binary performs only DataFrame column checking, while full type checkers (mypy, pyright, ty) analyze all
Python types. Use both: the binary for fast iteration, mypy for comprehensive checking.

**Note:** ty (Astral) does not currently support mypy plugins, so use the standalone binary for column checking with ty.

---

## Comparison

### Feature Matrix (Static Analysis Focus)

Comprehensive comparison of pandas/DataFrame typing and validation tools. **typedframes focuses on static analysis**
—catching errors at lint-time before your code runs.

| Feature                         | typedframes            | Pandera     | strictly_typed_pandas | pandas-stubs | dataenforce | pandas-type-checks | StaticFrame      | narwhals |
|---------------------------------|------------------------|-------------|-----------------------|--------------|-------------|--------------------|------------------|----------|
| **Version tested**              | 0.1.0                  | 0.29.0      | 0.3.6                 | 3.0.0        | 0.1.2       | 1.1.3              | 3.7.0            | 2.16.0   |
| **Analysis Type**               |
| When errors are caught          | **Static (lint-time)** | Runtime     | Static + Runtime      | Static       | Runtime     | Runtime            | Static + Runtime | Runtime  |
| **Static Analysis (our focus)** |
| Mypy plugin                     | ✅ Yes                  | ⚠️ Limited  | ✅ Yes                 | ✅ Yes        | ❌ No        | ❌ No               | ⚠️ Basic         | ❌ No     |
| Standalone checker              | ✅ Rust (~1ms)          | ❌ No        | ❌ No                  | ❌ No         | ❌ No        | ❌ No               | ❌ No             | ❌ No     |
| Column name checking            | ✅ Yes                  | ⚠️ Limited  | ✅ Yes                 | ❌ No         | ❌ No        | ❌ No               | ✅ Yes            | ❌ No     |
| Column type checking            | ✅ Yes                  | ⚠️ Limited  | ✅ Yes                 | ❌ No         | ❌ No        | ❌ No               | ✅ Yes            | ❌ No     |
| Typo suggestions                | ✅ Yes                  | ❌ No        | ❌ No                  | ❌ No         | ❌ No        | ❌ No               | ❌ No             | ❌ No     |
| **Runtime Validation**          |
| Data validation                 | ❌ No                   | ✅ Excellent | ✅ typeguard           | ❌ No         | ✅ Yes       | ✅ Yes              | ✅ Yes            | ❌ No     |
| Value constraints               | ❌ No                   | ✅ Yes       | ❌ No                  | ❌ No         | ❌ No        | ❌ No               | ✅ Yes            | ❌ No     |
| **Schema Features**             |
| Column grouping                 | ✅ ColumnSet            | ❌ No        | ❌ No                  | ❌ No         | ❌ No        | ❌ No               | ❌ No             | ❌ No     |
| Regex column matching           | ✅ Yes                  | ❌ No        | ❌ No                  | ❌ No         | ❌ No        | ❌ No               | ❌ No             | ❌ No     |
| **Backend Support**             |
| Pandas                          | ✅ Yes                  | ✅ Yes       | ✅ Yes                 | ✅ Yes        | ✅ Yes       | ✅ Yes              | ❌ Own            | ✅ Yes    |
| Polars                          | ✅ Yes                  | ✅ Yes       | ❌ No                  | ❌ No         | ❌ No        | ❌ No               | ❌ Own            | ✅ Yes    |
| DuckDB, cuDF, etc.              | ❌ No                   | ❌ No        | ❌ No                  | ❌ No         | ❌ No        | ❌ No               | ❌ No             | ✅ Yes    |
| **Project Status (Feb 2026)**   |
| Active development              | ✅ Yes                  | ✅ Yes       | ⚠️ Low                | ✅ Yes        | ❌ Inactive  | ⚠️ Low             | ✅ Yes            | ✅ Yes    |

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

### Type Checkers (Not DataFrame-Specific)

These are general Python type checkers. They don't validate DataFrame column names, but they can be used alongside
typedframes for comprehensive type checking:

- **[mypy](https://mypy-lang.org/)** (v1.19.1): The original Python type checker. typedframes provides a mypy plugin for
  column checking. See [performance benchmarks](#static-analysis-performance).

- **[ty](https://github.com/astral-sh/ty)** (v0.0.14, Astral): New Rust-based type checker, 10-60x faster than mypy on
  large codebases. Does not support mypy plugins—use typedframes standalone checker.

- **[pyrefly](https://pyrefly.org/)** (v0.51.1, Meta): Rust-based type checker from Meta, replacement for Pyre. Fast,
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

| Use Case                                             | Recommended Tool                       |
|------------------------------------------------------|----------------------------------------|
| Static column checking (existing pandas/polars)      | **typedframes**                        |
| Runtime data validation                              | Pandera                                |
| Both static + runtime                                | typedframes + Pandera (separate tools) |
| Cross-library portability (write once, run anywhere) | narwhals                               |
| Immutable DataFrames from scratch                    | StaticFrame                            |
| Pandas API type hints only                           | pandas-stubs                           |

---

## Type Safety With Multiple Backends

typedframes uses **explicit backend types** to ensure complete type safety:

```python
from typedframes import BaseSchema, Column
from typedframes.pandas import PandasFrame
from typedframes.polars import PolarsFrame
import pandas as pd
import polars as pl


class UserData(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)


# Pandas pipeline - type checker knows pandas methods
def pandas_analyze(df: PandasFrame[UserData]) -> PandasFrame[UserData]:
    return df[df['user_id'] > 100]  # ✓ Pandas syntax


# Polars pipeline - type checker knows polars methods
def polars_analyze(df: PolarsFrame[UserData]) -> PolarsFrame[UserData]:
    return df.filter(pl.col('user_id') > 100)  # ✓ Polars syntax


# Type checker prevents mixing backends
df_pandas = PandasFrame.from_schema(pd.read_csv("data.csv"), UserData)
df_polars: PolarsFrame[UserData] = pl.read_csv("data.csv")

pandas_analyze(df_pandas)  # ✓ OK
polars_analyze(df_polars)  # ✓ OK
pandas_analyze(df_polars)  # ✗ Type error: Expected PandasFrame, got PolarsFrame
```

---

## Features

### Clean Schema Definition

```python
from typedframes import BaseSchema, Column, ColumnSet, ColumnGroup


class TimeSeriesData(BaseSchema):
    timestamp = Column(type=str)
    temperature = ColumnSet(type=float, members=r"temp_sensor_\d+", regex=True)
    pressure = ColumnSet(type=float, members=r"pressure_\d+", regex=True)

    # Logical grouping
    sensors = ColumnGroup(members=[temperature, pressure])
```

### Beautiful Runtime API

```python
import pandas as pd
from typedframes.pandas import PandasFrame

df = PandasFrame.from_schema(pd.read_csv("sensors.csv"), TimeSeriesData)

# Access column groups as DataFrames
temps = df.temperature  # All temp_sensor_* columns
all_sensors = df.sensors  # All sensor columns

# Clean operations
avg_temp = df.temperature.mean()
max_pressure = df.pressure.max()

# Standard pandas access still works
df['timestamp']  # Single column
df[['timestamp', 'temp_sensor_1']]  # Multi-column select
```

### Column-Level Static Checking

```python
from typedframes.pandas import PandasFrame


def daily_summary(data: PandasFrame[TimeSeriesData]) -> PandasFrame[DailySummary]:
    # Type checker validates column access
    data['timestamp']  # ✓ OK - column exists
    data['date']  # ✗ Error: Column 'date' not in TimeSeriesData

    # Type checker validates ColumnSet access
    temps = data.temperature  # ✓ OK - ColumnSet exists
    summary = temps.mean()
    return summary
```

### Dynamic Column Matching

Perfect for time-series data where column counts change:

```python
class SensorReadings(BaseSchema):
    timestamp = Column(type=str)
    # Matches: sensor_1, sensor_2, ..., sensor_N
    sensors = ColumnSet(type=float, members=r"sensor_\d+", regex=True)

# Works regardless of how many sensor columns exist
df = PandasFrame.from_schema(pd.read_csv("readings_2024_01.csv"), SensorReadings)  # 50 sensors
df.sensors.mean()  # All sensor columns

df = PandasFrame.from_schema(pd.read_csv("readings_2024_02.csv"), SensorReadings)  # 75 sensors
df.sensors.mean()  # All sensor columns (different count, same code)
```

---

## Advanced Usage

### Merges, Joins, and Filters

Schema-typed DataFrames preserve their type through common operations:

**Pandas:**

```python
from typedframes import BaseSchema, Column
from typedframes.pandas import PandasFrame
import pandas as pd


class UserSchema(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)


class OrderSchema(BaseSchema):
    order_id = Column(type=int)
    user_id = Column(type=int)
    total = Column(type=float)


# Schema preserved through filtering
def get_active_users(df: PandasFrame[UserSchema]) -> PandasFrame[UserSchema]:
    return df[df['user_id'] > 100]  # ✓ Still PandasFrame[UserSchema]


# Schema preserved through column selection
def select_ids(df: PandasFrame[UserSchema]) -> PandasFrame[UserSchema]:
    return df[['user_id', 'email']]  # ✓ Still PandasFrame[UserSchema]


# Schema preserved through merges
users: PandasFrame[UserSchema] = ...
orders: PandasFrame[OrderSchema] = ...
merged = pd.merge(users, orders, on='user_id')  # Works as expected
merged = users.merge(orders, on='user_id')       # Also works
```

**Polars:**
```python
from typedframes.polars import PolarsFrame
import polars as pl


# Schema columns work in filter expressions
def filter_users(df: PolarsFrame[UserSchema]) -> pl.DataFrame:
    return df.filter(UserSchema.user_id.col > 100)


# Schema columns work in join expressions
def join_data(
    users: PolarsFrame[UserSchema],
    orders: PolarsFrame[OrderSchema]
) -> pl.DataFrame:
    return users.join(
        orders,
        left_on=UserSchema.user_id.col,
        right_on=OrderSchema.user_id.col
    )


# Schema columns work in select expressions
def select_columns(df: PolarsFrame[UserSchema]) -> pl.DataFrame:
    return df.select([UserSchema.user_id.col, UserSchema.email.col])
```

### Schema Algebra

When you merge, concat, or subset DataFrames, the resulting schema changes. Schema algebra lets you describe these
transformations and type the result — using column references instead of strings, so typos are caught immediately.

```python
from typedframes import BaseSchema, Column
from typedframes.pandas import PandasFrame
import pandas as pd


class Users(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)
    password_hash = Column(type=str)


class Orders(BaseSchema):
    order_id = Column(type=int)
    user_id = Column(type=int)
    total = Column(type=float)


# Combine schemas for merge/concat results
UserOrders = Users + Orders
# UserOrders has: user_id, email, password_hash, order_id, total

merged: PandasFrame[UserOrders] = pd.merge(users_df, orders_df, on="user_id")


# Select specific columns (column references, not strings)
UserBasic = Users.select([Users.user_id, Users.email])
# UserBasic has: user_id, email

subset: PandasFrame[UserBasic] = users_df[["user_id", "email"]]


# Drop columns
UserPublic = Users.drop([Users.password_hash])
# UserPublic has: user_id, email

public: PandasFrame[UserPublic] = users_df.drop(columns=["password_hash"])
```

Overlapping columns with the same type are allowed (common after merges). Conflicting types raise `SchemaConflictError`.

See [`examples/schema_algebra_example.py`](examples/schema_algebra_example.py) for a complete walkthrough.

---

## Examples

### Basic CSV Processing

```python
from typedframes import BaseSchema, Column
from typedframes.pandas import PandasFrame
import pandas as pd


class Orders(BaseSchema):
    order_id = Column(type=int)
    customer_id = Column(type=int)
    total = Column(type=float)
    date = Column(type=str)


def calculate_revenue(orders: PandasFrame[Orders]) -> float:
    return orders['total'].sum()


df = PandasFrame.from_schema(pd.read_csv("orders.csv"), Orders)
revenue = calculate_revenue(df)
```

### Time Series Analysis

```python
from typedframes import BaseSchema, Column, ColumnSet, ColumnGroup
from typedframes.pandas import PandasFrame
import pandas as pd


class SensorData(BaseSchema):
    timestamp = Column(type=str)
    temperature = ColumnSet(type=float, members=r"temp_\d+", regex=True)
    humidity = ColumnSet(type=float, members=r"humidity_\d+", regex=True)

    all_sensors = ColumnGroup(members=[temperature, humidity])


df = PandasFrame.from_schema(pd.read_csv("sensors.csv"), SensorData)

# Clean, type-safe operations
avg_temp_per_row = df.temperature.mean(axis=1)
all_readings_stats = df.all_sensors.describe()
```

### Multi-Step Pipeline

```python
from typedframes import BaseSchema, Column
from typedframes.pandas import PandasFrame
import pandas as pd


class RawSales(BaseSchema):
    date = Column(type=str)
    product_id = Column(type=int)
    quantity = Column(type=int)
    price = Column(type=float)


class AggregatedSales(BaseSchema):
    date = Column(type=str)
    total_revenue = Column(type=float)
    total_quantity = Column(type=int)


def aggregate_daily(df: PandasFrame[RawSales]) -> PandasFrame[AggregatedSales]:
    result = df.groupby('date').agg({
        'price': 'sum',
        'quantity': 'sum'
    }).reset_index()

    result.columns = ['date', 'total_revenue', 'total_quantity']
    return PandasFrame.from_schema(result, AggregatedSales)


# Type-safe pipeline
raw = PandasFrame.from_schema(pd.read_csv("sales.csv"), RawSales)
aggregated = aggregate_daily(raw)


# Type checker validates schema transformations
def analyze(df: PandasFrame[AggregatedSales]) -> float:
    return df['total_revenue'].mean()  # ✓ OK
    # df['price']  # ✗ Error: 'price' not in AggregatedSales
```

### Polars Performance Pipeline

```python
from typedframes import BaseSchema, Column
from typedframes.polars import PolarsFrame
import polars as pl


class LargeDataset(BaseSchema):
    id = Column(type=int)
    value = Column(type=float)
    category = Column(type=str)


def efficient_aggregation(df: PolarsFrame[LargeDataset]) -> pl.DataFrame:
    return (
        df.filter(LargeDataset.value.col > 100)
        .group_by('category')
        .agg(LargeDataset.value.col.mean())
    )


# Polars handles large files efficiently
df: PolarsFrame[LargeDataset] = pl.read_csv("huge_file.csv")
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

### Explicit Backend Types

We use explicit `PandasFrame` and `PolarsFrame` types because:
- Pandas and polars have different APIs
- Type safety requires knowing which methods are available
- Being explicit prevents bugs

**We reject:**
- ❌ "Universal DataFrame" abstractions (you lose features)
- ❌ Implicit backend detection (runtime errors)
- ❌ Lowest-common-denominator APIs

The reason to choose polars over pandas is its lazy evaluation, native parallelism, and expressive query syntax. If you wrap it in an abstraction that only exposes shared features, you lose the advantages that made polars worthwhile. typedframes lets you use each library's full API while still getting schema-level type safety.

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
from typedframes.polars import PolarsFrame

class SalesData(BaseSchema):
    revenue = Column(type=float)

def process(df: PolarsFrame[SalesData]) -> PolarsFrame[SalesData]:
    return df.filter(df['revnue'] > 100)  # ✗ Error at lint-time: 'revnue' not in SalesData
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
- [x] Schema Algebra (`SchemaA + SchemaB`, `.select()`, `.drop()`)
- [x] Column name collision warnings

**Planned:**

- [ ] **Pandera integration** - `to_pandera_schema(MySchema)` to convert typedframes schemas to Pandera schemas for
  runtime validation
- [ ] **Optional runtime validation** - `Field` class with constraints (`gt`, `ge`, `lt`, `le`) on Column definitions,
  opt-in validation at data load time
- [ ] **IDE Integration (.pyi stubs)** - Generate `.pyi` stub files for schema definitions to enable autocomplete in
  IDEs (VSCode, PyCharm) without running the type checker

---

## FAQ

**Q: Do I need to choose between pandas and polars?**
A: No. Define your schema once, use it with both. Just use the appropriate type (`PandasFrame` or `PolarsFrame`) in your function signatures.

**Q: Does this replace Pandera?**
A: No, it complements it. Use typedframes for static analysis, Pandera for runtime validation.

**Q: Is the standalone checker required?**
A: No. You can use just the mypy plugin, just the standalone checker, or both. They catch the same errors.

**Q: What about pyright/pylance users?**
A: The mypy plugin doesn't work with pyright yet. Use the standalone checker (`typedframes check`) for now. Pyright plugin is on the roadmap.

**Q: Does this work with existing pandas/polars code?**
A: Yes. You can gradually adopt typedframes by adding schemas to new code. Existing code continues to work.

**Q: What if my column name conflicts with a pandas/polars method?**
A: Avoid using column names that match reserved DataFrame methods like `mean`, `sum`, `filter`, `select`, `head`,
`tail`, etc. These will shadow the method when accessed via attribute syntax:

```python
class BadSchema(BaseSchema):
    mean = Column(type=float)  # ⚠️ Shadows df.mean()
    filter = Column(type=str)  # ⚠️ Shadows df.filter()


# This calls your column, not the method!
df.mean  # Returns the 'mean' column, not the mean() method

# Use bracket syntax for methods when column names conflict
df['mean'].mean()  # Access column 'mean', then call mean() method
```

The typedframes linter will warn you about these conflicts. Best practice is to use more descriptive names like
`mean_value` or `filter_type`.

---

## Credits

Built by developers who believe DataFrame bugs should be caught at lint-time, not in production.

Inspired by the needs of ML/data science teams working with complex data pipelines.

---

**Questions? Issues? Ideas?** [Open an issue](https://github.com/yourusername/typedframes/issues)

**Ready to catch DataFrame bugs before runtime?** `pip install typedframes`

# typedframes

**Static analysis for pandas and polars DataFrames. Catch column errors at lint-time, not runtime.**

```python
from src.typedframes import BaseSchema, Column
from src.typedframes import PandasFrame


class UserData(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)
    signup_date = Column(type=str)


def process(df: PandasFrame[UserData]) -> None:
    df['user_id']  # ✓ OK
    df['username']  # ✗ Error: Column 'username' not in UserData
```

---

## Why typedframes?

**The problem:** Many pandas bugs are column mismatches. You access a column that doesn't exist, pass the wrong schema to a function, or make a typo. These errors only surface at runtime, often in production.

**The solution:** Define your DataFrame schemas as Python classes. Get static type checking that catches column errors before you even run your code.

**What you get:**

- ✅ **Static analysis** - Catch column errors at lint-time with mypy or the standalone checker
- ✅ **Beautiful runtime UX** - `df.column_group.mean()` instead of ugly column lists
- ✅ **Works with pandas AND polars** - Same schema API, explicit backend types
- ✅ **Dynamic column matching** - Regex-based ColumnSets for time-series data
- ✅ **Zero runtime overhead** - No validation slowdown (use Pandera if you need that)
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
from src.typedframes import BaseSchema, Column, ColumnSet


class SalesData(BaseSchema):
    date = Column(type=str)
    revenue = Column(type=float)
    customer_id = Column(type=int)

    # Dynamic columns with regex
    metrics = ColumnSet(type=float, members=r"metric_\d+", regex=True)
```

### Use With Pandas

```python
from src.typedframes import PandasFrame

# Load data with schema
df: PandasFrame[SalesData] = SalesData().read_csv("sales.csv")

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
from src.typedframes import PolarsFrame

# Same schema, different backend
df: PolarsFrame[SalesData] = SalesData(backend="polars").read_csv("sales.csv")

# Same clean attribute API
print(df.revenue.sum())
print(df.metrics.mean())


# Type-safe polars operations
def analyze_polars(data: PolarsFrame[SalesData]) -> float:
    return data.select(['revenue']).mean()
    # return data.select(['profit'])  # ✗ Error at lint-time


# Polars methods work as expected
filtered = df.filter(df['revenue'] > 1000)
grouped = df.group_by('customer_id').agg(pl.col('revenue').sum())
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
plugins = ["typedframes.mypy_plugin"]

# Or mypy.ini
[mypy]
plugins = typedframes.mypy_plugin

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

## Type Safety With Multiple Backends

typedframes uses **explicit backend types** to ensure complete type safety:

```python
from src.typedframes import BaseSchema, Column
from src.typedframes import PandasFrame
from src.typedframes import PolarsFrame
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
df_pandas = UserData().read_csv("data.csv")
df_polars = UserData(backend="polars").read_csv("data.csv")

pandas_analyze(df_pandas)  # ✓ OK
polars_analyze(df_polars)  # ✓ OK
pandas_analyze(df_polars)  # ✗ Type error: Expected PandasFrame, got PolarsFrame
```

### Backend-Agnostic Code

For code that works with both backends, use the schema's attribute API:

```python
from typing import Union

def summary(df: Union[PandasFrame[UserData], PolarsFrame[UserData]]) -> dict:
    # Schema attributes work for both backends
    return {
        'mean_id': df.user_id.mean(),
        'unique_emails': df.email.nunique()
    }

# Works with both
summary(df_pandas)  # ✓ OK
summary(df_polars)  # ✓ OK
```

---

## Features

### Clean Schema Definition

```python
from src.typedframes import BaseSchema, Column, ColumnSet, ColumnGroup


class TimeSeriesData(BaseSchema):
    timestamp = Column(type=str)
    temperature = ColumnSet(type=float, members=r"temp_sensor_\d+", regex=True)
    pressure = ColumnSet(type=float, members=r"pressure_\d+", regex=True)

    # Logical grouping
    sensors = ColumnGroup(members=[temperature, pressure])
```

### Beautiful Runtime API

```python
from src.typedframes import PandasFrame

df: PandasFrame[TimeSeriesData] = TimeSeriesData().read_csv("sensors.csv")

# Access column groups as DataFrames
temps = df.temperature  # All temp_sensor_* columns
all_sensors = df.sensors  # All sensor columns

# Clean operations
avg_temp = df.temperature.mean()
max_pressure = df.pressure.max()

# Standard pandas/polars access still works
df['timestamp']  # Single column
df[['timestamp', 'temp_sensor_1']]  # Multi-column select
```

### Column-Level Static Checking

```python
from src.typedframes import PandasFrame


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
df = SensorReadings().read_csv("readings_2024_01.csv")  # 50 sensors
df.sensors.mean()  # All sensor columns

df = SensorReadings().read_csv("readings_2024_02.csv")  # 75 sensors
df.sensors.mean()  # All sensor columns (different count, same code)
```

---

## Advanced Usage

### Dynamic Column Aliasing

```python
from src.typedframes import DefinedLater


class FlexibleSchema(BaseSchema):
    id_column = Column(type=int, alias=DefinedLater)
    value_columns = ColumnSet(members=DefinedLater)


# Set at runtime before use
FlexibleSchema.id_column.alias = "customer_id"
FlexibleSchema.value_columns.members = [f"metric_{i}" for i in range(10)]

df = FlexibleSchema().read_csv("data.csv")
customer_ids = df.id_column  # Accesses 'customer_id' column
```

### Working With Both Backends

```python
from src.typedframes import PandasFrame
from src.typedframes import PolarsFrame


class ProcessingPipeline:
    """Use polars for heavy lifting, pandas for ML"""

    def load_and_filter(self, path: str) -> PolarsFrame[RawData]:
        # Polars is faster for large CSV files
        df = RawData(backend="polars").read_csv(path)
        return df.filter(df['value'] > 100)

    def prepare_for_ml(self, df: PolarsFrame[RawData]) -> PandasFrame[RawData]:
        # Convert to pandas for sklearn
        pandas_df = df.to_pandas()
        return RawData().from_df(pandas_df)

    def train(self, df: PandasFrame[RawData]) -> None:
        # ML libraries expect pandas
        from sklearn.ensemble import RandomForestRegressor
        model = RandomForestRegressor()
        model.fit(df[['value']], df['target'])
```

### Integration With Pandera

Use typedframes for schemas and static analysis, Pandera for runtime validation:

```python
from src.typedframes import BaseSchema, Column
from src.typedframes import PandasFrame
from src.typedframes import to_pandera_schema
import pandera as pa


class UserData(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)
    age = Column(type=int)


# Get static analysis from typedframes
def process(df: PandasFrame[UserData]) -> None:
    df['user_id']  # ✓ Static checking works
    df['name']  # ✗ Error at lint-time


# Get runtime validation from Pandera when needed
pandera_schema = to_pandera_schema(UserData)

df = pd.read_csv("users.csv")
validated_df = pandera_schema.validate(df)  # Runtime checks

# Best of both worlds: static + runtime checking
```

### Optional Runtime Validation

```python
from src.typedframes import Column, Field


class UserData(BaseSchema):
    user_id = Column(type=int, validators=[Field(gt=0)])
    age = Column(type=int, validators=[Field(ge=0, le=120)])
    email = Column(type=str)


# Validation is opt-in
df = UserData().read_csv("users.csv", validate=True)
# Raises ValidationError if data doesn't match constraints
```

---

## Examples

### Basic CSV Processing

```python
from src.typedframes import BaseSchema, Column
from src.typedframes import PandasFrame


class Orders(BaseSchema):
    order_id = Column(type=int)
    customer_id = Column(type=int)
    total = Column(type=float)
    date = Column(type=str)


def calculate_revenue(orders: PandasFrame[Orders]) -> float:
    return orders['total'].sum()


df = Orders().read_csv("orders.csv")
revenue = calculate_revenue(df)
```

### Time Series Analysis

```python
from src.typedframes import BaseSchema, Column, ColumnSet, ColumnGroup
from src.typedframes import PandasFrame


class SensorData(BaseSchema):
    timestamp = Column(type=str)
    temperature = ColumnSet(type=float, members=r"temp_\d+", regex=True)
    humidity = ColumnSet(type=float, members=r"humidity_\d+", regex=True)

    all_sensors = ColumnGroup(members=[temperature, humidity])


df: PandasFrame[SensorData] = SensorData().read_csv("sensors.csv")

# Clean, type-safe operations
avg_temp_per_row = df.temperature.mean(axis=1)
all_readings_stats = df.all_sensors.describe()
```

### Multi-Step Pipeline

```python
from src.typedframes import BaseSchema, Column
from src.typedframes import PandasFrame


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
    return AggregatedSales().from_df(result)


# Type-safe pipeline
raw = RawSales().read_csv("sales.csv")
aggregated = aggregate_daily(raw)


# Type checker validates schema transformations
def analyze(df: PandasFrame[AggregatedSales]) -> float:
    return df['total_revenue'].mean()  # ✓ OK
    # df['price']  # ✗ Error: 'price' not in AggregatedSales
```

### Polars Performance Pipeline

```python
from src.typedframes import BaseSchema, Column
from src.typedframes import PolarsFrame
import polars as pl


class LargeDataset(BaseSchema):
    id = Column(type=int)
    value = Column(type=float)
    category = Column(type=str)


def efficient_aggregation(df: PolarsFrame[LargeDataset]) -> PolarsFrame[LargeDataset]:
    # Polars lazy evaluation
    return (
        df.filter(pl.col('value') > 100)
        .group_by('category')
        .agg(pl.col('value').mean())
    )


# Polars handles large files efficiently
df = LargeDataset(backend="polars").read_csv("huge_file.csv")
result = efficient_aggregation(df)
```

---

## Comparison

### vs Pandera

| Feature | typedframes | Pandera |
|---------|-------------|---------|
| Static analysis | ✅ Full support | ⚠️ Limited/broken |
| Runtime validation | ⚠️ Optional | ✅ Excellent |
| Schema API | ✅ Clean (`Column`) | ⚠️ Verbose (`Series[T]`) |
| Column grouping | ✅ ColumnSet/ColumnGroup | ❌ No |
| Polars support | ✅ Yes (explicit types) | ✅ Yes |
| Backend type safety | ✅ Yes | ❌ No |
| Performance overhead | ✅ Zero | ⚠️ Validation cost |

**Use typedframes for:** Static analysis, clean schemas, development-time checking  
**Use Pandera for:** Runtime validation, production data quality checks  
**Use both together:** typedframes schemas + Pandera validation via integration

### vs pandas-stubs

| Feature | typedframes | pandas-stubs |
|---------|-------------|--------------|
| Column-level types | ✅ Yes | ❌ No |
| API-level types | ✅ Yes (via stubs) | ✅ Yes |
| Runtime benefits | ✅ Clean access | ❌ None |
| Schema definition | ✅ Built-in | ❌ Manual |

**typedframes builds on pandas-stubs** - we provide column-level checking while pandas-stubs provides API-level types.

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
- ❌ Lowest-common-denominator APIs (why use polars then?)

### Why No Built-in Validation?

**Validation belongs at data ingestion, not in Python.**

If you're validating DataFrames in Python, your data pipeline needs fixing.

That said, we provide Pandera integration for cases where runtime validation is genuinely necessary.

---

## Contributing

Contributions welcome! See [CONTRIBUTING.md](CONTRIBUTING.md)

**Priority areas:**
- Transformation tracking (select, rename, join, merge)
- Better error messages
- More examples and documentation
- Performance optimizations

**Future possibilities:**
- Dask support
- Modin support  
- Ray datasets support
- Pyright plugin

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

**In Progress:**
- [ ] Transformation tracking
- [ ] Better error messages
- [ ] More comprehensive examples

**Future:**
- [ ] Pyright plugin
- [ ] Auto-generated .pyi stubs
- [ ] Dask/Modin support
- [ ] Integration with more validation libraries

---

## FAQ

**Q: Do I need to choose between pandas and polars?**  
A: No. Define your schema once, use it with both. Just use the appropriate type (`PandasFrame` or `PolarsFrame`) in your function signatures.

**Q: Can I write backend-agnostic functions?**  
A: Yes, using `Union[PandasFrame[T], PolarsFrame[T]]` or by using only the schema attribute API (`df.column_name`).

**Q: Does this replace Pandera?**  
A: No, it complements it. Use typedframes for static analysis, Pandera for runtime validation. We provide integration helpers.

**Q: Is the standalone checker required?**  
A: No. You can use just the mypy plugin, just the standalone checker, or both. They catch the same errors.

**Q: What about pyright/pylance users?**  
A: The mypy plugin doesn't work with pyright yet. Use the standalone checker (`typedframes check`) for now. Pyright plugin is on the roadmap.

**Q: Does this work with existing pandas/polars code?**  
A: Yes. You can gradually adopt typedframes by adding schemas to new code. Existing code continues to work.

---

## Credits

Built by developers who believe DataFrame bugs should be caught at lint-time, not in production.

Inspired by the needs of ML/data science teams working with complex data pipelines.

---

**Questions? Issues? Ideas?** [Open an issue](https://github.com/yourusername/typedframes/issues)

**Ready to catch DataFrame bugs before runtime?** `pip install typedframes`
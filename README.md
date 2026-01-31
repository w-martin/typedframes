# Pandas Column Linter (Experimental)

A high-performance Rust-based linter for pandas DataFrame column schemas, integrated with `mypy`.

> [!WARNING]
> This project is currently **experimental**. API and behavior may change.

## Features

- **Static Analysis**: Catch column mismatches without running your code.
- **High Performance**: Built in Rust for near-instant linting.
- **Multiple Schema Supports**:
  - `pandera.DataFrameModel`
  - `typing.TypedDict`
  - `pandandic.BaseFrame`
- **Advanced Tracking**:
  - Detects accessing non-existent columns via subscript `df["col"]` or attribute `df.col`.
  - Support for `pandandic` features: `Column` (with `alias`), `ColumnSet`, and `ColumnGroup`.
  - Detects column name typos with fuzzy matching suggestions.
  - Mutation tracking: Warns when adding columns not defined in the schema.
  - Supports `DataFrame[Schema]` generic syntax and type hints.

## Installation

```bash
pip install pandas-column-linter
```

## Configuration

Enable or disable the linter in your `pyproject.toml`:

```toml
[tool.pandas_column_linter]
enabled = true
```

Add the plugin to your `mypy` configuration:

```toml
[tool.mypy]
plugins = [
  "pandas_column_linter.mypy"
]
```

## Examples

The examples are located in the `examples/` directory and have their own environment. To run them:

```bash
cd examples
uv sync
uv run mypy pandera_example.py
```

### Plugin Output Examples

When running `mypy`, you will see informative errors like these:

```text
examples/pandera_example.py:40: error: Column 'name' does not exist in UserSchema (defined at line 5)  [misc]
examples/pandera_example.py:44: error: Column 'new_column' does not exist in UserSchema (mutation tracking)  [misc]
examples/pandera_example.py:49: error: Column 'emai' does not exist in UserSchema (defined at line 5) (did you mean 'email'?)  [misc]
```

### Advanced Features

- **Merge & Concat Support**: The linter tracks combined schemas from `merge` and `concat` operations.
- **Informative Errors**: Error messages tell you exactly where the schema was defined.
- **Fuzzy Matching**: Suggestions for typos based on Levenshtein distance.
- **Mutation Tracking**: Alerts you when adding columns to a DataFrame that aren't in its original schema.

### Pandera

```python
import pandera as pa
from pandera.typing import DataFrame, Series

class UserSchema(pa.DataFrameModel):
    user_id: Series[int]
    email: Series[str]

def process(df: DataFrame[UserSchema]):
    print(df["emai"])  # Error: Column 'emai' does not exist (did you mean 'email'?)
    df["new_col"] = 1  # Error: Column 'new_col' does not exist (mutation tracking)
```

### TypedDict

```python
from typing import TypedDict
from pandas import DataFrame

class UserSchema(TypedDict):
    user_id: int
    email: str

def process(df: DataFrame[UserSchema]):
    print(df["name"])  # Error: Column 'name' does not exist in UserSchema
```

### Pandandic

```python
from pandandic import BaseFrame, Column, ColumnSet

class UserFrame(BaseFrame):
    user_id = Column(type=int)
    email = Column(type=str, alias="email_address")
    metadata = ColumnSet(members=["age", "gender"])

df = UserFrame().read_csv("users.csv")
print(df.user_id)        # OK
print(df.email_address)  # OK (alias)
print(df.age)            # OK (ColumnSet member)
print(df.missing)        # Error: Column 'missing' does not exist in UserFrame
```

## Development

### Rust Linter

```bash
cd rust_pandas_linter
# For CLI use
cargo build --release
# For Python extension use
maturin develop
```

### Python Plugin

```bash
python -m unittest discover tests
```

## License

MIT

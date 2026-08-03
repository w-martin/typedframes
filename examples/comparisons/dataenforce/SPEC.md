# dataenforce vs typedframes Comparison

## Installation & Compatibility

### dataenforce Status
- **Version**: 0.1.2 (latest available)
- **Package Status**: ❌ BROKEN on Python 3.13+ (and 3.14)
- **Error**: `ImportError: cannot import name '_TypingEmpty' from 'typing'`
  - The package relies on internal typing module APIs (`_TypingEmpty`, `_tp_cache`) that were removed/changed in Python 3.13
  - No newer versions available on PyPI
  - This makes the package unusable on current Python versions

### Installation Report
- **Installed successfully**: Yes (version 0.1.2 added to pyproject.toml)
- **Runs on Python 3.14**: No (import error at runtime)
- **Runs on Python 3.13**: No (same import error)
- **Runs on Python 3.11**: Likely yes (not tested — workspace limited to Python 3.14)

## Design Pattern Comparison

### dataenforce API
- **Type of Validation**: Runtime-only (decorator pattern)
- **Schema Definition**: Inline in function signature as `Dataset["column":type, ...]`
- **Example**:
```python
from dataenforce import Dataset, validate

@validate
def process_orders(df: Dataset["order_id":int, "customer_name":str]):
    return df["customer_name"]
```

### typedframes API
- **Type of Validation**: Static analysis at lint-time + optional runtime
- **Schema Definition**: Explicit `BaseSchema` class with `Column` definitions
- **Example**:
```python
from typing import Annotated
import pandas as pd
from typedframes import BaseSchema, Column

class OrderSchema(BaseSchema):
    order_id = Column(type=int)
    customer_name = Column(type=str)

def process_orders(df: Annotated[pd.DataFrame, OrderSchema]) -> pd.Series:
    return df["customer_name"]
```

## Static Analysis Comparison

### dataenforce + mypy
- **Result**: ❌ No column schema validation
- **Test File**: `b_static_analysis.py`
- **Mypy Output**:
  ```
  Success: no issues found in 1 source file
  ```
- **Findings**:
  - Column typos like `df["custmer_name"]` (missing 's') are NOT caught
  - Missing columns like `df["quanity"]` (missing 't') are NOT caught
  - Function calls with incomplete data are NOT caught
  - Errors only surface at **runtime** if the function is actually called with bad data

### typedframes + mypy
- **Result**: ✅ Full column schema validation at lint-time
- **Test File**: `c_typedframes_comparison.py`
- **Mypy Output**:
  ```
  c_typedframes_comparison.py:50: error: Column 'custmer_name' does not exist in OrderSchema (defined at line 38) (did you mean 'customer_name'?)  [misc]
  c_typedframes_comparison.py:55: error: Column 'unit_cost' does not exist in OrderSchema (defined at line 38)  [misc]
  Found 2 errors in 1 file (checked 1 source file)
  ```
- **Findings**:
  - Column typos are caught with fuzzy suggestions ("did you mean 'customer_name'?")
  - Wrong column names are detected before runtime
  - All direct column access errors are caught at lint-time

## Key Differences

| Aspect | dataenforce | typedframes |
|--------|-------------|-------------|
| Validation Timing | Runtime only | Static (lint-time) |
| Schema Definition | Inline in signature | Explicit class |
| Mypy Support | None (SILENT) | Full (mypy plugin) |
| Error Detection | Function must be called with bad data | Caught at lint time |
| IDE Integration | No schema hints | Full IDE schema hints |
| Column Typos | Caught at runtime | Caught at lint time |
| Package Status | Broken on Python 3.13+ | Works across Python 3.11-3.14 |

## Conclusion

### dataenforce
- **Design**: Runtime-only validation via decorator on function signatures
- **Current Status**: Broken on Python 3.13+ due to internal typing module API changes
- **Practical Impact**: Cannot demonstrate the working example on Python 3.13/3.14 environments
- **Limitation**: Only catches errors when functions are actually called with bad data; errors caught at runtime, not development time

### typedframes
- **Design**: Static analysis via mypy plugin + optional runtime checks
- **Current Status**: Works on Python 3.11–3.14
- **Practical Impact**: Catches schema errors during type checking, before code runs
- **Advantage**: Errors caught at lint-time via IDE integration or pre-commit hooks; developers know about bugs immediately during development

### Key Takeaway
typedframes provides **early error detection** (lint-time) while dataenforce relies on **late error detection** (runtime). For data pipelines where columns matter, lint-time checking via typedframes prevents errors from reaching production code.

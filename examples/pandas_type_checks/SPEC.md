# pandas-type-checks vs typedframes Comparison

## Overview

This directory contains example files demonstrating the key differences between **pandas-type-checks** and **typedframes** for DataFrame schema validation.

**pandas-type-checks**: Runtime-only validation via decorator
**typedframes**: Static analysis (mypy plugin) + runtime checking

---

## What is pandas-type-checks?

A runtime decorator library that validates DataFrame and Series arguments at function call time.

### Key Characteristics

- **Runtime-only**: No static type analysis capability
- **Decorator-based**: Apply `@pandas_type_check()` to functions
- **Column validation**: Checks column names and dtypes at call time
- **Strict/non-strict modes**: Control whether extra columns are allowed
- **Works with pandas/Series**: Validated via `DataFrameArgument` and `SeriesArgument`

### What It Catches

✓ **Wrong dtype at runtime**: e.g., `int64` instead of `float64`
✓ **Missing required columns at runtime**: Column referenced in schema but missing
✓ **Extra columns (non-strict mode)**: Allowed by default

### What It Misses

✗ **Column name typos**: `df["custmer_name"]` vs `df["customer_name"]` — caught only at runtime when the column is accessed
✗ **Static analysis**: mypy sees no column information; cannot detect access errors before execution
✗ **Type narrowing**: No IDE autocomplete for column names
✗ **Structural typing**: Cannot distinguish schemas at static check time

---

## How typedframes Differs

typedframes uses a **mypy plugin** to provide **static column-aware type checking** while maintaining pandas/polars compatibility.

### Key Characteristics

- **Static + Runtime**: mypy plugin catches errors at development time
- **Annotation-based**: Use `Annotated[pd.DataFrame, Schema]` with type hints
- **Column-aware mypy**: Plugin understands schema and validates column access
- **Multi-dataframe**: Supports pandas, polars, and other DataFrames
- **Schema-first**: Define schemas as Python classes with `Column()` fields

### What It Catches (Statically)

✓ **Column name typos**: `df["custmer_name"]` flagged by mypy before runtime
✓ **Unknown columns**: `df["nonexistent_col"]` caught statically
✓ **Schema mismatches**: Wrong schema passed to function
✓ **Type-aware operations**: Some dtype-based checks (with type info)

### Example

```python
from typing import Annotated
import pandas as pd
from typedframes import BaseSchema, Column

class OrderSchema(BaseSchema):
    order_id = Column(type=int)
    customer_name = Column(type=str)

def process_orders(orders: Annotated[pd.DataFrame, OrderSchema]) -> pd.Series:
    orders_checked: Annotated[pd.DataFrame, OrderSchema] = orders
    # ✓ mypy catches typo statically:
    return orders_checked.groupby("custmer_name")["total"].sum()
    #                              ^^^^^^^^^^^^
    #                              Error: Column 'custmer_name' does not exist
```

---

## File Guide

### a_working_example.py

**Demonstrates pandas-type-checks in action.**

- Define schema with `DataFrameArgument(name, dtype_dict)`
- Apply `@pandas_type_check(schema)` decorator
- Valid data passes silently
- Invalid data raises `TypeError` at runtime

**Run**: `python3 a_working_example.py`
**Output**: Shows runtime validation catching dtype/column errors

### b_static_analysis.py

**Shows mypy without the typedframes plugin.**

- Functions with intentional column typos: `"custmer_name"`, `"sku"`, `"qty"`
- Uses pandas-type-checks (runtime only)
- mypy sees no schema information

**Run**: `uv run mypy b_static_analysis.py --config-file mypy.ini 2>&1`
**Output**: `Success: no issues found` — mypy is SILENT on column errors

### c_typedframes_comparison.py

**Same logic, but with typedframes and the mypy plugin.**

- Uses `Annotated[pd.DataFrame, Schema]` type hints
- Creates local variables with annotations to trigger plugin checking
- Same intentional typos as `b_static_analysis.py`

**Run**: `uv run mypy c_typedframes_comparison.py --config-file mypy_typedframes.ini 2>&1`
**Output**: mypy REPORTS column errors statically:
```
c_typedframes_comparison.py:70: error: Column 'sku' does not exist in OrderSchema
```

---

## Installation & Setup

```bash
# Install pandas-type-checks and typedframes
uv add "pandas-type-checks>=1.1.0" pandas mypy
uv add --dev "typedframes[mypy]>=0.2.1"

# mypy configuration files provided:
# - mypy.ini: Standard mypy (no plugin)
# - mypy_typedframes.ini: With typedframes.mypy plugin
```

### Installed Version

- **pandas-type-checks**: 1.1.3
- **typedframes**: 0.2.1
- **pandas**: 2.3.3
- **mypy**: 2.1.0

---

## Key Findings

### What pandas-type-checks is Good For

1. **Runtime validation of DataFrame structure** without external schema libraries
2. **Simple decorator-based API** — easy to add to existing code
3. **Lazy validation** — only checks when function is called with bad data
4. **No build overhead** — pure Python, no compilation

### Its Limitations

1. **Zero static analysis** — mypy cannot see column names
2. **Errors at runtime** — typos only caught when code path executes
3. **No IDE support** — no autocomplete for column names
4. **Development friction** — must run full test suite to catch schema errors

### When to Use Each

| Scenario | pandas-type-checks | typedframes |
|----------|-------------------|-------------|
| Quick runtime validation | ✓ | ✓ |
| Catch typos before runtime | ✗ | ✓ |
| Static column awareness | ✗ | ✓ |
| IDE autocomplete | ✗ | ✓ |
| Development experience | Poor | Excellent |
| Production reliability | Good | Excellent |

---

## Active Development Status

⚠️ **pandas-type-checks**: Low activity. Last release November 2024. Issue/PR response rate minimal. No major pandas 3.x compatibility work visible.

✓ **typedframes**: Active development. Latest release June 2026. Ongoing mypy plugin enhancements, multi-dataframe support.

---

## Recommendation

For most projects requiring DataFrame schema validation:
- **Development**: Use typedframes with mypy plugin for static checking
- **Runtime**: typedframes provides both static + runtime validation
- **Legacy projects**: pandas-type-checks works but requires comprehensive testing

The mypy plugin approach trades zero runtime overhead for excellent development experience.

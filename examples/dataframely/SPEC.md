# Dataframely vs. Typedframes Comparison

## Overview

This directory contains comparison examples showing the differences between **dataframely** (polars-only, runtime validation with generic type parameters) and **typedframes** (column-level static analysis and mypy integration).

## Key Finding

**Dataframely's "mypy support" is generic parameter checking only, NOT column-level analysis.**

- `dy.DataFrame[OrderSchema]` vs `dy.DataFrame[OtherSchema]` are validated as different types by mypy
- But `df["custmer_name"]` (typo) on a `dy.DataFrame[OrderSchema]` is completely silent in mypy
- This is because the `DataFrame` class does not override `__getitem__`, so mypy uses polars' signature which returns `Any`

## Environment

- **dataframely**: 2.10.1
- **polars**: 1.41.2
- **mypy**: 2.1.0
- **typedframes**: 0.2.1

## Files

### a_working_example.py

Idiomatic dataframely usage demonstrating:
- Schema definition with `dy.Schema` base class
- DataFrame validation at runtime with `.validate()`
- Type-annotated functions accepting `dy.DataFrame[Schema]`
- Value constraints via `@dy.rule` decorators

**Status**: Runs successfully

### b_static_analysis.py

Demonstrates what mypy catches and misses with dataframely:

```python
class OrderSchema(dy.Schema):
    order_id = dy.Int64()
    customer_name = dy.String()
    # ...

def process(df: dy.DataFrame[OrderSchema]) -> None:
    x = df["custmer_name"]   # TYPO — mypy is SILENT
    y = df["unit_cost"]      # WRONG NAME — mypy is SILENT
    z = df["customer_name"]  # CORRECT — not validated by mypy

def wrong_schema(df: dy.DataFrame[OtherSchema]) -> None:
    process(df)  # mypy CATCHES THIS (generic parameter mismatch)
```

**Mypy output** (standard config, no plugin):
```
Success: no issues found in 1 source file
```

**Findings:**
- ✗ Column subscript typos (`df["custmer_name"]`) are NOT caught
- ✗ Wrong column names (`df["unit_cost"]`) are NOT caught
- ✓ Schema type mismatches ARE caught (generic parameter)
- ✓ All errors detected only at runtime

### c_typedframes_comparison.py

Shows typedframes catching column-level errors via its mypy plugin:

```python
class OrderSchema(BaseSchema):
    customer_name = Column(type=str)
    unit_price = Column(type=float)
    # ...

def process_orders_with_typos() -> None:
    df: Annotated[pl.DataFrame, OrderSchema] = pl.DataFrame({...})

    x = df["custmer_name"]   # TYPO — caught by plugin
    y = df["unit_cost"]      # WRONG — caught by plugin
```

**Mypy output** (with typedframes plugin):
```
c_typedframes_comparison.py:68: error: Column 'custmer_name' does not exist in OrderSchema (defined at line 54) (did you mean 'customer_name'?)  [misc]
c_typedframes_comparison.py:72: error: Column 'unit_cost' does not exist in OrderSchema (defined at line 54)  [misc]
Found 2 errors in 1 file
```

**Findings:**
- ✓ Column subscript typos are caught at type-check time
- ✓ Missing columns are caught with helpful suggestions ("did you mean")
- ✓ Works with both string access (`df["col"]`) and polars expressions (`pl.col("col")`)

## Summary

| Feature | Dataframely | Typedframes |
|---------|-------------|-------------|
| Runtime schema validation | ✓ | ✓ |
| Generic type parameters (T[Schema1] vs T[Schema2]) | ✓ | ✓ |
| Column-level mypy checking | ✗ | ✓ |
| Catches subscript typos (`df["typo"]`) | ✗ (runtime only) | ✓ (static) |
| Polars support | ✓ | ✓ |
| Pandas support | ✗ | ✓ |
| Mypy plugin | No | Yes |

## Usage Notes

To run mypy with typedframes plugin:
```bash
uv run mypy <file> --config-file mypy_typedframes.ini
```

To run typedframes static checker:
```bash
uv run python -m typedframes.cli check <file>
```

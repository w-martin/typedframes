# Pandera Comparison Study

## Overview

This package compares **pandera** (Python data validation library) with **typedframes** (schema-based DataFrame type checker) for static analysis of column access errors.

**Package:** `pandera`
**Version:** 0.29.0
**Purpose:** Data validation and schema definition for pandas DataFrames using declarative models

## What Pandera Does

Pandera allows defining reusable schemas for DataFrame validation:
- Declarative schema definition via `pa.DataFrameModel`
- Runtime validation of data against schema
- Type annotations with `pandera.typing.DataFrame[SchemaType]`
- Mypy plugin for basic type checking

## Static Analysis Results

### What Pandera Catches

✅ **Wrong schema type passed to function**
```python
def process_orders(df: DataFrame[OrderSchema]) -> pd.Series:
    ...

wrong_df: DataFrame[WrongSchema] = ...
process_orders(wrong_df)  # ✅ mypy CATCHES this
```
Error: `Argument 1 to "process_orders" has incompatible type "DataFrame[WrongSchema]"; expected "DataFrame[OrderSchema]"`

### What Pandera Misses

❌ **Column name typos**
```python
df: DataFrame[OrderSchema] = ...
df["custmer_name"]  # ❌ NOT caught (no error from pandera mypy plugin)
```
This would only fail at runtime with `KeyError`.

❌ **Non-existent column access**
```python
df: DataFrame[OrderSchema] = ...
df["unit_cost"]  # ❌ NOT caught (should be "unit_price")
```
Also fails silently at type-check time.

## Typedframes Comparison

### What Typedframes Catches

✅ **Column name typos** (with suggestions)
```python
df: Annotated[pd.DataFrame, OrderSchema] = ...
df["custmer_name"]
# Error: Column 'custmer_name' does not exist in OrderSchema
# (did you mean 'customer_name'?)
```

✅ **Non-existent column access**
```python
df: Annotated[pd.DataFrame, OrderSchema] = ...
df["unit_cost"]
# Error: Column 'unit_cost' does not exist in OrderSchema
# (did you mean 'unit_price'?)
```

### Key Differences

| Feature | Pandera | Typedframes |
|---------|---------|-------------|
| Schema definition | Declarative models | BaseSchema class |
| Type annotation | `DataFrame[Schema]` | `Annotated[DataFrame, Schema]` |
| Mypy plugin | ✅ Yes | ✅ Yes |
| Catches wrong schema type | ✅ Yes | ✅ Yes |
| Catches column typos | ❌ No | ✅ Yes (with suggestions) |
| Catches non-existent columns | ❌ No | ✅ Yes (with suggestions) |
| Runtime validation | ✅ Yes | ❌ No (static only) |

## Files

- `a_working_example.py` — Idiomatic pandera usage with sample data
- `b_static_analysis.py` — Demonstrates pandera's mypy plugin strengths and gaps
- `c_typedframes_comparison.py` — Same errors caught by typedframes plugin
- `mypy.ini` — Config for pandera's mypy plugin
- `mypy_typedframes.ini` — Config for typedframes' mypy plugin

## Mypy Output

**Pandera plugin** (detects schema type mismatch only):
```
b_static_analysis.py:82: error: Argument 1 to "process_orders" has
  incompatible type "DataFrame[WrongSchema]"; expected "DataFrame[OrderSchema]"
  [arg-type]
Found 1 error in 1 file (checked 1 source file)
```
Column name errors at lines 85, 89 are NOT caught.

**Typedframes plugin** (detects all column access errors):
```
c_typedframes_comparison.py:65: error: Column 'custmer_name' does not exist
  in OrderSchema (defined at line 51) (did you mean 'customer_name'?)
  [misc]
c_typedframes_comparison.py:69: error: Column 'unit_cost' does not exist
  in OrderSchema (defined at line 51)  [misc]
Found 2 errors in 1 file (checked 1 source file)
```

## Summary

Pandera's mypy plugin provides **coarse-grained type safety** (schema type matching) but lacks **fine-grained column access checking**. Typedframes' mypy plugin provides both, catching column name typos and non-existent access with helpful suggestions. However, pandera includes runtime validation (catch errors at execution time), while typedframes focuses on static analysis only.

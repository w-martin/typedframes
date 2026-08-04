# Patito Comparison with typedframes

## Overview

**patito** (v0.8.6) is a lightweight DataFrame validation library built on Pydantic. It provides runtime schema validation for DataFrames, with Polars as the primary backend.

## Key Findings

### Runtime Validation

patito provides runtime validation through `.validate()`:
- **Polars**: Full support, catches missing columns and type mismatches at runtime
- **Pandas**: Works via automatic conversion to Polars (requires undeclared pyarrow dependency)

Example:
```python
class OrderModel(pt.Model):
    order_id: int
    customer_name: str

df = pl.DataFrame({...})
validated = OrderModel.validate(df)  # Raises DataFrameValidationError if invalid
```

### Static Analysis

**No static type checking support.**

- No `py.typed` marker in package
- No mypy type stubs
- mypy reports: "module installed but missing library stubs or py.typed marker"
- Column access typos and non-existent columns are **not caught** at lint-time

Example of missed errors:
```python
validated = OrderModel.validate(df)
x = validated["custmer_name"]   # Typo — mypy says nothing
y = validated["unit_cost"]      # Wrong column — mypy says nothing
```

## Comparison with typedframes

| Feature | patito | typedframes |
|---------|--------|-------------|
| Runtime validation | ✓ | ✓ (via validators) |
| Polars support | ✓ | ✓ |
| Pandas support | ✓ (via pyarrow) | ✓ |
| Static type checking | ✗ | ✓ (mypy plugin) |
| Column error detection | Runtime only | Lint-time |
| Type hints provided | No | Yes |
| py.typed marker | No | Yes |

### typedframes Advantages

1. **Static Analysis**: Catches typos and missing columns at lint-time via mypy plugin
2. **Type Safety**: Full type hint support for IDE autocomplete
3. **Composable**: Use alongside runtime validators, not instead of

### patito Advantages

1. **Simpler API**: Minimal boilerplate, built on Pydantic
2. **Runtime Flexibility**: Validation can be conditional or deferred
3. **Direct Use**: No need for special type annotations

## Test Results

### a_working_example.py

✓ Polars validation (success and failures detected)
✓ Pandas validation (success and failures detected)

### b_static_analysis.py

```
$ mypy b_static_analysis.py --config-file mypy_strict.ini
b_static_analysis.py:9: error: Skipping analyzing "patito": module is
  installed, but missing library stubs or py.typed marker [import-untyped]
Found 1 error in 1 file (checked 1 source file)
```

**Result**: Column access errors not caught. patito has no static analysis.

### c_typedframes_comparison.py

```
$ mypy c_typedframes_comparison.py --config-file mypy_typedframes.ini
c_typedframes_comparison.py:52: error: Column 'custmer_name' does not exist
  in OrderSchema (defined at line 34) (did you mean 'customer_name'?)  [misc]
c_typedframes_comparison.py:57: error: Column 'unit_cost' does not exist
  in OrderSchema (defined at line 34)  [misc]
c_typedframes_comparison.py:62: error: Column 'completely_made_up' does
  not exist in OrderSchema (defined at line 34)  [misc]
Found 3 errors in 1 file (checked 1 source file)
```

**Result**: All three column access errors caught at lint-time by typedframes mypy plugin.

## Summary

patito is a good choice if you:
- Need simple runtime validation with Polars/Pandas
- Don't care about static analysis

Use **typedframes** if you want:
- Lint-time column error detection
- Static type safety
- IDE autocomplete for DataFrame columns

Note: an earlier version of this document claimed DuckDB support/status for
both patito and typedframes, backed by a `test_duckdb.py` script that does not
exist in this directory and never has (per git history). Neither library has
DuckDB-specific support, so those claims have been removed rather than left
unverifiable.

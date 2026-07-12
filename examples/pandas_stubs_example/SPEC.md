# pandas-stubs vs typedframes: Type Checking Comparison

## Setup

- **pandas-stubs version**: 3.0.3.260530
- **pandas version**: 3.0.3
- **mypy version**: 2.1.0
- **typedframes version**: 0.3.0
- **Python**: ≥3.11

## Summary

**pandas-stubs** provides API-level type checking for pandas methods but has **zero column-level awareness**. **typedframes** bridges this gap with schema-based column-level type checking via a mypy plugin.

## Detailed Findings

### 1. API-Level Type Checking (pandas-stubs ✓)

pandas-stubs successfully validates:
- DataFrame/Series method signatures
- Return types for operations like `groupby()`, `merge()`
- Type consistency (e.g., `.sum()` on numeric vs. string Series)

**Evidence**: `a_working_example.py` runs successfully, demonstrating that pandas-stubs understands pandas method signatures and return types.

### 2. Column Access: pandas-stubs (✗)

When running mypy with pandas-stubs on `b_static_analysis.py`:

```
Success: no issues found in 1 source file
```

However, at runtime:

```python
y = df["custmer_name"]        # TYPO: missing 'o'
z = df["unit_cost"]           # WRONG NAME (should be unit_price)
w = df["completely_made_up"]  # NONEXISTENT COLUMN
```

All three raise `KeyError` at runtime, but **mypy reports no errors**.

**Why**: `pd.DataFrame` accepts arbitrary column names as strings. pandas-stubs has no way to know which columns should exist, so it allows any string key and returns `pd.Series[Any]`.

### 3. Column Access: typedframes (✓)

When running mypy with the typedframes plugin on `c_typedframes_comparison.py`:

```
c_typedframes_comparison.py:49: error: Column 'custmer_name' does not exist in OrderSchema (defined at line 33) (did you mean 'customer_name'?)  [misc]
c_typedframes_comparison.py:54: error: Column 'unit_cost' does not exist in OrderSchema (defined at line 33)  [misc]
c_typedframes_comparison.py:59: error: Column 'completely_made_up' does not exist in OrderSchema (defined at line 33)  [misc]
Found 3 errors in 1 file (checked 1 source file)
```

**All three errors caught**. typedframes even suggests the correct column name for the typo: `did you mean 'customer_name'?`

## Key Insight

| Aspect | pandas-stubs | typedframes |
|--------|-------------|------------|
| DataFrame method signatures | ✓ Knows types | ✓ Knows types |
| Return types (groupby, merge) | ✓ Validated | ✓ Validated |
| Column existence | ✗ No awareness | ✓ Schema-based |
| Typos in column names | ✗ Not caught | ✓ Caught |
| Nonexistent columns | ✗ Not caught | ✓ Caught |
| Fuzzy suggestions | — | ✓ Did you mean X? |

## Conclusion

- **pandas-stubs** is useful for validating pandas API usage (method signatures, return types).
- **pandas-stubs** cannot catch column-level bugs (typos, missing columns).
- **typedframes** solves the column-level problem via schema annotations and a mypy plugin.
- For data pipelines where columns matter, use **typedframes** alongside pandas-stubs for comprehensive type safety.

# Strictly-Typed-Pandas vs Typedframes Comparison

## Setup

- **stp version**: 0.3.7
- **typedframes version**: 0.2.1
- **pandas**: 2.2.3
- **polars**: 1.41.2
- **mypy**: 2.1.0

## Key Findings

### strictly-typed-pandas Static Analysis

**Result**: NO — mypy does NOT catch column name errors

```
$ uv run mypy b_static_analysis.py --config-file mypy_stp_nopl.ini
Success: no issues found in 1 source file
```

**Why**: strictly-typed-pandas has no mypy plugin. It relies entirely on pandas-stubs type hints, which treat DataFrame `__getitem__` as returning `Any`. Column names are not type-checked at lint-time.

### typedframes Static Analysis

**Result**: YES — mypy catches all column name errors with suggestions

```
$ uv run mypy c_typedframes_comparison.py --config-file mypy_typedframes.ini

c_typedframes_comparison.py:64: error: Column 'custmer_name' does not exist in
  OrderSchema (defined at line 60) (did you mean 'customer_name'?)  [misc]
c_typedframes_comparison.py:69: error: Column 'unit_cost' does not exist in
  OrderSchema (defined at line 60)  [misc]
c_typedframes_comparison.py:83: error: Column 'custmer_name' does not exist in
  OrderSchema (defined at line 80) (did you mean 'customer_name'?)  [misc]
c_typedframes_comparison.py:86: error: Column 'unit_cost' does not exist in
  OrderSchema (defined at line 80)  [misc]
Found 4 errors in 1 file
```

## Comparison Matrix

| Feature | strictly-typed-pandas | typedframes |
|---------|----------------------|-------------|
| **Column typo detection (mypy)** | ❌ No | ✅ Yes |
| **Typo suggestions** | N/A | ✅ Yes ("did you mean") |
| **pandas support** | ✅ Yes | ✅ Yes |
| **polars support** | ❌ No | ✅ Yes |
| **mypy plugin** | ❌ No | ✅ Yes (Rust-based) |
| **Runtime validation** | ✅ Yes (via DataSet) | ❌ No (lint-only) |
| **Active development** | Unclear (last update?) | ✅ Active |
| **Lines of code (core)** | Medium | Medium (Rust component) |

## Why the Difference?

### strictly-typed-pandas
- Uses **class annotations** to define schemas
- Provides **runtime validation** via `DataSet[Schema]` subclass
- No mypy plugin means type information is lost at lint-time
- pandas-stubs treats DataFrame `__getitem__` as `Any`
- Good for catching errors at runtime, not lint-time

### typedframes
- Uses **`Column` objects** to define schemas
- Integrates **custom mypy plugin** with Rust-based checker
- Catches column errors at lint-time, before runtime
- Supports both pandas and polars
- Rust checker is invoked during mypy plugin execution
- Type information is precise: knows exact column names

## Files in this Example

- `a_working_example.py` — Idiomatic strictly-typed-pandas usage (runs successfully)
- `b_static_analysis.py` — Intentional errors; shows stp does NOT catch them in mypy
- `c_typedframes_comparison.py` — Same errors; shows typedframes DOES catch them in mypy
- `d_debug_typedframes.py` — Minimal example to verify typedframes checker works
- `mypy_stp_nopl.ini` — mypy config for strictly-typed-pandas (no plugin)
- `mypy_typedframes.ini` — mypy config for typedframes (with plugin)

## How to Run

```bash
# Check strictly-typed-pandas (catches nothing)
uv run mypy b_static_analysis.py --config-file mypy_stp_nopl.ini

# Check typedframes (catches column errors)
uv run mypy c_typedframes_comparison.py --config-file mypy_typedframes.ini

# Run working example
uv run python a_working_example.py
```

## Conclusion

**strictly-typed-pandas** and **typedframes** solve the same problem in fundamentally different ways:

1. **strictly-typed-pandas**: Validation at runtime (catch errors when you run the code)
2. **typedframes**: Detection at lint-time (catch errors when you write the code)

For projects that already have strict mypy configurations, **typedframes** is the clear choice. It catches typos *before* code is executed, with helpful suggestions. strictly-typed-pandas is useful if you prefer runtime validation and don't need lint-time checking.

# Narwhals vs. Typedframes Comparison

This example compares **narwhals** (backend portability) with **typedframes** (type safety), demonstrating that they solve different problems and are complementary.

## Files

### a_working_example.py
Demonstrates narwhals' core strength: write once, run on any backend.

```bash
uv run python a_working_example.py
```

Shows the same transformation working identically on pandas and polars.

### b_static_analysis.py
Shows narwhals' blind spot: column name errors pass silently through mypy.

```bash
uv run mypy b_static_analysis.py --config-file mypy.ini 2>&1
```

Expected: `Success: no issues found in 1 source file`

Narwhals functions contain intentional typos (`custmer_name` instead of `customer_name`) that mypy does NOT catch because strings are dynamically accepted.

### c_typedframes_comparison.py
Shows that typedframes catches the errors narwhals misses.

```bash
uv run mypy c_typedframes_comparison.py --config-file mypy_typedframes.ini 2>&1
```

With the typedframes mypy plugin enabled, column access errors are caught at lint-time.

## Key Insight

| Tool | Solves | Does Not Solve |
|------|--------|----------------|
| **Narwhals** | Backend portability (pandas → polars → duckdb) | Type safety for column names |
| **Typedframes** | Type safety for column names | Backend portability |

**Conclusion**: These tools are complementary, not competing. Use narwhals for portability and typedframes for type safety.

## Project Setup

```bash
uv sync
uv run python a_working_example.py  # Run example
uv run mypy b_static_analysis.py --config-file mypy.ini 2>&1  # Test narwhals
```

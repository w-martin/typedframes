# Narwhals vs. Typedframes: Orthogonal Solutions

## Overview

**Narwhals** and **typedframes** solve different problems and are complementary tools:

- **Narwhals**: Provides **backend portability** — write once, run on pandas/polars/duckdb/etc.
- **Typedframes**: Provides **static type safety** — catch column name errors at lint-time

## Key Findings

### Narwhals Version
- `2.24.0`

### Test Results

#### Column Typos with Narwhals (b_static_analysis.py)

Command:
```bash
uv run mypy b_static_analysis.py --config-file mypy.ini 2>&1
```

Result: **No errors detected** ✗

```
Success: no issues found in 1 source file
```

Narwhals functions with column name errors pass mypy silently:
- `df["custmer_name"]` — typo (should be `customer_name`)
- `df["unit_cost"]` — wrong name (should be `unit_price`)
- `df["nonexistent"]` — completely non-existent

**Analysis**: Narwhals accepts any column name as a string at type-check time. Column access errors are only caught at runtime. Mypy has no way to know what columns are valid since `nw.DataFrame` is fully dynamic.

---

#### Column Typos with Typedframes (c_typedframes_comparison.py)

Command:
```bash
uv run mypy c_typedframes_comparison.py --config-file mypy_typedframes.ini 2>&1
```

Result:
```
c_typedframes_comparison.py:67: error: Column 'sku' does not exist in OrderSchema (defined at line 64)  [misc]
c_typedframes_comparison.py:83: error: Column 'nonexistent' does not exist in OrderSchema (defined at line 80)  [misc]
Found 2 errors in 1 file (checked 1 source file)
```

**Requires `pandas-stubs`**: without it, mypy cannot resolve pandas subscript access
(`df["col"]`) precisely enough for the typedframes plugin to hook in, and pandas-side
column errors go undetected — only the polars-side error (`process_polars`, above) would
be caught. `pandas-stubs>=3.0.0` is a direct dependency of this comparison for that reason.

**One known gap, confirmed here**: the pandas-side typo in `process_pandas` —
`df_checked.groupby("custmer_name")` — is **not** caught. typedframes currently tracks
column names passed to subscript access (`df["col"]`) and `pl.col()`-style expressions,
but not column names passed as `.groupby()` arguments. The two subscript-based errors
(`process_polars`'s `df_checked["sku"]` and `process_completely_missing`'s
`df_checked["nonexistent"]`) are both caught correctly.

---

## Complementary Use Cases

### Use Narwhals When...
- You need backend-agnostic code
- You want to support multiple DataFrame libraries (pandas → polars migration, etc.)
- Runtime flexibility is more important than static safety
- Your team uses different backends in different projects

### Use Typedframes When...
- You care about catching column name errors early (development time, not runtime)
- You use a single backend (pandas or polars) and want static verification
- You want IDE autocompletion and mypy checking for column access
- Type safety and developer experience are priorities

### Use Both Together
```python
# typedframes defines the schema statically
class OrderSchema(BaseSchema):
    order_id = Column(type=int)
    customer_name = Column(type=str)
    total = Column(type=float)


# Narwhals handles backend portability
import narwhals as nw


def process(df: Annotated[nw.DataFrame, OrderSchema]) -> nw.DataFrame:
    # Typedframes catches column errors before runtime
    # Narwhals works with any backend (pandas, polars, etc.)
    return df.filter(nw.col("total") > 50.0).select(["customer_name", "total"])
```

---

## Conclusion

| Aspect | Narwhals | Typedframes |
|--------|----------|-------------|
| **Type Safety** | None — strings are accepted | Full — schema violations caught at lint-time |
| **Portability** | Excellent — backend-agnostic | Per-backend (pandas/polars via annotation) |
| **Error Detection** | Runtime | Development-time (mypy plugin) |
| **Learning Curve** | Simple | Requires mypy integration |

**They are not competing tools** — they solve different problems. Typedframes handles static analysis; narwhals handles portability. Teams can use both effectively together.

# StaticFrame vs typedframes: Type Safety Tradeoff

## Quick Answer

**StaticFrame:** Achieves type safety by replacing pandas entirely. Requires full code rewrite.

**typedframes:** Achieves type safety by annotating existing pandas/polars code. Zero API changes, gradual migration.

---

## The Tradeoff

### StaticFrame Approach

StaticFrame is an immutable DataFrame library designed from the ground up with type safety in mind.

**Pros:**
- Immutable by design (eliminates whole classes of bugs)
- Built-in type system via its own ecosystem
- No ambiguity about mutable operations

**Cons:**
- ❌ **Cannot use existing pandas code** — must rewrite everything
- ❌ **Not compatible with pandas/polars ecosystems** — libraries expect pandas.DataFrame
- ❌ **Column access is runtime string-based** — `frame['column_name']` is not statically typed
  - mypy cannot catch `frame['custmer_name']` typos
  - Similar issue to pandas: column names are strings
- ❌ **Breaking change for all existing code**
- ❌ **Learning curve** — different API for all DataFrame operations

**What mypy catches with StaticFrame:**
- Function signature mismatches (e.g., passing wrong frame type)
- Operations on Series/Frame return types
- ❌ Does NOT catch column name typos (e.g., `frame['custmer_name']`)
- ❌ Does NOT catch schema mismatches for string-based column access

---

### typedframes Approach

typedframes adds schema annotations to existing pandas/polars code via `Annotated[pd.DataFrame, Schema]`.

**Pros:**
- ✅ **Works with existing pandas code** — zero rewrites
- ✅ **Gradual migration** — annotate one function at a time
- ✅ **Compatible with pandas ecosystem** — all libraries expecting `pd.DataFrame` work
- ✅ **Column name checking via mypy plugin** — catches `df['custmer_name']` typos
- ✅ **Zero runtime overhead** — annotations are purely for type checking
- ✅ **Use with polars, cuDF, DuckDB** — works with any pandas-like API

**Cons:**
- Column access is still runtime string-based (but mypy plugin catches typos)
- Requires mypy plugin configuration
- Plugin is lightweight but still a tool to install

**What mypy catches with typedframes:**
- ✅ Column name typos (e.g., `df['custmer_name']` → error)
- ✅ Schema mismatches between functions
- ✅ Missing required columns in operations
- Runtime check at DataFrame construction ensures schema compliance

---

## Comparison Table

| Feature | StaticFrame | typedframes |
|---------|------------|------------|
| **Requires rewrite** | ❌ Yes | ✅ No |
| **Existing pandas code** | ❌ No | ✅ Yes |
| **Library compatibility** | ❌ Limited | ✅ Full pandas ecosystem |
| **Column name type safety** | ❌ No | ✅ Yes (mypy plugin) |
| **Immutability** | ✅ Enforced | ⚠️ Developer responsibility |
| **API familiarity** | ❌ New | ✅ Standard pandas |
| **Gradual adoption** | ❌ All-or-nothing | ✅ Function-by-function |

---

## Example: Column Name Typo

### With typedframes + mypy plugin:

```python
from typing import Annotated
import pandas as pd
from typedframes import BaseSchema, Column

class OrderSchema(BaseSchema):
    order_id = Column(type=int)
    customer_name = Column(type=str)

def process(df: Annotated[pd.DataFrame, OrderSchema]) -> None:
    # Caught by mypy plugin ✅
    df['custmer_name']  # ERROR: did you mean 'customer_name'?
```

### With StaticFrame:

```python
import static_frame as sf

def process(frame: sf.Frame) -> None:
    # NOT caught by mypy ❌
    # Column names are runtime strings, not statically typed
    frame['custmer_name']  # Runtime KeyError, not type error
```

---

## When to Use Each

### Use **StaticFrame** if:
- You're starting a new project from scratch
- Immutability is critical for your use case
- You don't have existing pandas dependencies
- Your team is willing to learn a new API

### Use **typedframes** if:
- You have existing pandas code (almost always)
- You want to add type safety incrementally
- You need compatibility with the pandas ecosystem
- You want zero API changes to your codebase
- You're using pandas, polars, cuDF, or other pandas-like libraries

---

## Implementation Notes

### Example Files

- **a_working_example.py** — Demonstrates StaticFrame idiomatic usage (immutable, Frame-based)
- **b_static_analysis.py** — Shows what mypy catches with StaticFrame (column typos: NO)
- **c_typedframes_comparison.py** — Side-by-side comparison with actual code examples
- **d_checker_demo.py** — Minimal demo showing typedframes checker detection
- **test_plugin.py** — Test file for verifying checker behavior
- **mypy.ini** — Standard mypy configuration (no plugins needed)
- **mypy_typedframes.ini** — Configuration with typedframes plugin (Note: still requires checker)
- **SPEC.md** — This document

### Running the Examples

```bash
# Show StaticFrame working example (runs without errors)
python a_working_example.py

# Check static analysis with mypy (StaticFrame, no schema checking)
mypy b_static_analysis.py --config-file mypy.ini

# Run typedframes/StaticFrame comparison demo
python c_typedframes_comparison.py

# Run typedframes checker to see column error detection
typedframes check d_checker_demo.py

# Run typedframes checker in strict mode
typedframes check test_plugin.py --strict
```

---

## Actual Test Results

### StaticFrame v5.0.0

**Question:** Does mypy catch `df['custmer_name']` typo with StaticFrame?

**Answer:** No. The typo is not caught.

```bash
$ mypy b_static_analysis.py --config-file mypy.ini
Success: no issues found in 1 source file
```

StaticFrame's column access uses string subscription (`frame['column_name']`), which is not statically typed. The `__getitem__` method accepts any string, so typos are runtime errors, not type errors.

### typedframes Column Checker

**Question:** Does the typedframes checker catch `df['custmer_name']` typo?

**Answer:** Yes. The error is caught with a helpful suggestion.

```bash
$ typedframes check d_checker_demo.py
d_checker_demo.py:35:15: error[unknown-column] Column 'custmer_name' does not exist in OrderSchema (defined at line 28) (did you mean 'customer_name'?)
✗ Found 1 error in 1 file (0.0s)
```

**Key requirement:** Must annotate locally with `Annotated[pd.DataFrame, Schema]` to trigger checking:

```python
df_annot: Annotated[pd.DataFrame, OrderSchema] = df  # Triggers checker
result = df_annot['custmer_name']  # ERROR: detected
```

---

## Conclusion

**StaticFrame:** Type safety by replacement (full rewrite, immutability).
- Does NOT catch column name typos at type-check time
- All column access is runtime string-based

**typedframes:** Type safety by annotation (zero rewrite, gradual adoption).
- DOES catch column name typos via standalone checker
- Works with existing pandas code via annotations
- Checker suggests correct column names on typos

Choose based on whether you can rewrite your codebase (StaticFrame) or need to work with existing code (typedframes).

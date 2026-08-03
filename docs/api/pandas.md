# pandas

## Recommended: `Annotated` type annotation

For most use cases, annotate with `Annotated[pd.DataFrame, MySchema]` and use native
pandas subscript access. The checker validates all column references at lint time without
any runtime overhead:

```python
from typing import Annotated
import pandas as pd
from typedframes import BaseSchema, Column

class OrderSchema(BaseSchema):
    order_id   = Column(type=int)
    amount     = Column(type=float)
    status     = Column(type=str)

df: Annotated[pd.DataFrame, OrderSchema] = pd.read_csv("orders.csv")
print(df["order_id"])          # ✓ native pandas, validated by checker
print(df[OrderSchema.amount.s])  # ✓ refactor-safe via .s descriptor
```

## `PandasFrame` — deprecated runtime enhancement

!!! warning "Deprecated"
    `PandasFrame` is deprecated and will be removed in a future release. Use
    `Annotated[pd.DataFrame, Schema]` (above) instead — the static checker validates
    column access identically either way, without a runtime subclass.

    `PandasFrame` and its polars counterpart, `PolarsFrame`, were never able to reach
    parity with each other: polars DataFrames are Rust objects that can't be
    meaningfully subclassed, so `PolarsFrame` has only ever been an alias for
    `Annotated[pl.DataFrame, Schema]` with no real runtime subclass behind it, while
    `PandasFrame` genuinely is one. That asymmetry is confusing on its own, and it
    also means `PolarsFrame`'s alias mechanism breaks Liskov substitution under
    strict type checking (assigning the plain `pl.DataFrame` it always actually is at
    runtime to a `PolarsFrame[Schema]`-annotated variable is flagged as a type error).
    `Annotated[...]` alone is sufficient for everything the static checker validates,
    without either maintenance-heavy runtime wrapper mechanism.

`PandasFrame` was a `pd.DataFrame` subclass that added runtime column validation and
descriptor dispatch (`df[Schema.column]`):

```python
from typedframes.pandas import PandasFrame
from typedframes import BaseSchema, Column, ColumnSet

class SalesSchema(BaseSchema):
    product_id  = Column(type=int)
    region_cols = ColumnSet(members=r"region_\w+", type=float, regex=True)

# Deprecated -- emits a DeprecationWarning; prefer Annotated[pd.DataFrame, Schema]
df = PandasFrame.from_schema(pd.read_csv("sales.csv"), SalesSchema)
```

---

::: typedframes.pandas.PandasFrame

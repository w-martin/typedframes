# polars

## Recommended: `Annotated` type annotation

For most use cases, annotate with `Annotated[pl.DataFrame, MySchema]` and use native
polars expressions. The checker validates both subscript access and `pl.col()` references
at lint time:

```python
from typing import Annotated
import polars as pl
from typedframes import BaseSchema, Column

class EventSchema(BaseSchema):
    event_id  = Column(type=int)
    user_id   = Column(type=int)
    timestamp = Column(type=str)

df: Annotated[pl.DataFrame, EventSchema] = pl.read_csv("events.csv")

# Native polars — both forms validated by checker
print(df.select(pl.col("event_id")))             # ✓ pl.col() validated
print(df.filter(pl.col("timestamp").is_not_null()))  # ✓ pl.col() in filter
print(df.select(pl.col("typo")))                  # ✗ unknown-column — 'typo' not in EventSchema

# Descriptor access — refactor-safe polars expressions
df.select(EventSchema.event_id.col, EventSchema.user_id.col)
df.filter(EventSchema.user_id.col > 100)
```

## `PolarsFrame` — deprecated alias

!!! warning "Deprecated"
    `PolarsFrame` is deprecated and will be removed in a future release. Use
    `Annotated[pl.DataFrame, Schema]` directly (above) instead — `PolarsFrame[Schema]`
    has always just been an alias for exactly that, with no real runtime subclass
    behind it (polars DataFrames are Rust objects that can't be meaningfully
    subclassed). Under strict type checking, `PolarsFrame[Schema]` is declared as a
    nominal `pl.DataFrame` subclass so it gets full autocomplete — but that means
    assigning the plain `pl.DataFrame` it actually always is at runtime looks like a
    Liskov substitution violation to a type checker. `Annotated[...]` doesn't have
    this problem, since it's transparent to both mypy and the runtime.

`PolarsFrame[Schema]` was a spelling alias for `Annotated[pl.DataFrame, Schema]`:

```python
from typedframes.polars import PolarsFrame
from typedframes import BaseSchema, Column

class EventSchema(BaseSchema):
    event_id = Column(type=int)

# Deprecated -- emits a DeprecationWarning; prefer Annotated[pl.DataFrame, Schema]
df: PolarsFrame[EventSchema] = pl.read_csv("events.csv")
```

`PolarsFrame.read_csv(source, schema=Schema)` / `.read_parquet(...)` / `.read_json(...)`
/ `.read_excel(...)` are pass-throughs to the corresponding `pl.read_*` function —
`schema` is accepted for static checking only and is never validated or attached to
the result at runtime.

---

::: typedframes.polars.PolarsFrame

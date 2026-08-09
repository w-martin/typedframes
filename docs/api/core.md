# Core

The core module provides the building blocks for defining DataFrame schemas.

## Defining a schema

```python
from typing import Annotated
import pandas as pd
from typedframes import BaseSchema, Column


class UserSchema(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)
    region = Column(type=str)


# Annotate with the schema — the checker validates all downstream column access
df: Annotated[pd.DataFrame, UserSchema] = pd.read_csv("users.csv")
print(df["user_id"])  # ✓ validated at lint time
print(df["username"])  # ✗ unknown-column: 'username' not in UserSchema
```

## Descriptor access

`Column` descriptors provide refactor-safe column name references:

```python
# .s — returns column name as str, for pandas subscript access
df[UserSchema.user_id.s]  # equivalent to df["user_id"]
df.groupby(UserSchema.region.s)

# .col — returns pl.Expr, for polars expressions
df.filter(UserSchema.email.col.is_not_null())
df.select(UserSchema.user_id.col, UserSchema.region.col)
```

## Column sets

`ColumnSet` captures groups of columns — by explicit list or regex pattern:

```python
from typedframes import ColumnSet


class MetricsSchema(BaseSchema):
    user_id = Column(type=int)
    score_cols = ColumnSet(members=r"score_\d+", type=float, regex=True)


# .s returns the list of matched names (raises ValueError for regex sets)
class ReportSchema(BaseSchema):
    user_id = Column(type=int)
    metadata = ColumnSet(members=["source", "campaign"], type=str)


df[ReportSchema.metadata.s]  # → ["source", "campaign"]
```

## Column groups

`ColumnGroup` bundles related `Column` or `ColumnSet` members under one name:

```python
from typedframes import ColumnGroup, ColumnSet


class SensorSchema(BaseSchema):
    timestamp = Column(type=str)
    temperatures = ColumnSet(members=r"temp_\d+", type=float, regex=True)
    pressures = ColumnSet(members=r"pressure_\d+", type=float, regex=True)

    # Bundle both sets for convenient selection
    all_sensors = ColumnGroup(members=[temperatures, pressures])


# .s returns flat list of names (raises ValueError for regex members)
# .cols() returns list of pl.Expr for polars selection
df.select(SensorSchema.all_sensors.cols())
```

---

::: typedframes.BaseSchema

::: typedframes.Column

::: typedframes.ColumnSet

::: typedframes.ColumnGroup

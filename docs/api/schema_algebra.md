# Schema Algebra

Compose schemas from smaller pieces using inheritance or the `+` operator. This is useful
for join results, enriched views, or any DataFrame that combines columns from multiple sources.

## Schema inheritance

```python
from typedframes import BaseSchema, Column


class OrderSchema(BaseSchema):
    order_id = Column(type=int)
    amount = Column(type=float)


class CustomerSchema(BaseSchema):
    customer_id = Column(type=int)
    name = Column(type=str)


class ReportSchema(OrderSchema, CustomerSchema):
    """Inherits all columns from both parents."""

    region = Column(type=str)  # add extra columns
```

## The `+` operator

`SchemaA + SchemaB` creates a merged schema with all columns from both:

```python
from typedframes import combine_schemas

ReportSchema = OrderSchema + CustomerSchema
# equivalent to multiple inheritance with no extra columns
```

The `+` operator raises `SchemaConflictError` if the same column name appears in both
schemas with different types.

## Merge key access

Use `.s` for the merge key when joining DataFrames:

```python
from typing import Annotated
import pandas as pd

JoinedSchema = OrderSchema + CustomerSchema

left: Annotated[pd.DataFrame, OrderSchema] = pd.read_csv("orders.csv")
right: Annotated[pd.DataFrame, CustomerSchema] = pd.read_csv("customers.csv")
merged: Annotated[pd.DataFrame, JoinedSchema] = left.merge(right, on=OrderSchema.order_id.s)
```

---

::: typedframes.combine_schemas

::: typedframes.SchemaConflictError

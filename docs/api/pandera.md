# Pandera Integration

`to_pandera_schema()` converts a `BaseSchema` into a
[Pandera](https://pandera.readthedocs.io/) `DataFrameSchema` for runtime data validation.

Use typedframes for **static analysis** (catching column errors at lint time) and Pandera
for **runtime validation** (checking data values, null constraints, and types against
actual data).

```python
from typing import Annotated
import pandas as pd
import pandera as pa
from typedframes import BaseSchema, Column
from typedframes.pandera import to_pandera_schema


class OrderSchema(BaseSchema):
    order_id = Column(type=int)
    amount = Column(type=float)
    status = Column(type=str)


# Static analysis: checker validates column access at lint time
def process(df: Annotated[pd.DataFrame, OrderSchema]) -> None:
    print(df["order_id"])  # ✓ validated by typedframes checker
    print(df["revenue"])  # ✗ unknown-column at lint time


# Runtime validation: Pandera validates actual data values
pandera_schema = to_pandera_schema(OrderSchema)
validated_df = pandera_schema.validate(pd.read_csv("orders.csv"))
```

!!! tip
    Install the Pandera extra to use this integration:
    ```shell
    pip install typedframes[pandera]
    ```

---

::: typedframes.pandera.to_pandera_schema

# typedframes

Static analysis for pandas and polars DataFrames. Catch column errors at lint-time, not runtime.

```python
from typing import Annotated
import pandas as pd
from typedframes import BaseSchema, Column


class UserData(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)
    signup_date = Column(type=str)


df: Annotated[pd.DataFrame, UserData] = pd.read_csv("users.csv")
df['user_id']    # ✓ Validated by checker
df['username']   # ✗ unknown-column: Column 'username' not in UserData
```

```shell
typedframes check src/
# ✓ Checked 12 files in 0.0s
# ✗ src/pipeline.py:3 - Column 'username' not in UserData
```

## How it works

The standalone checker (written in Rust) runs on any existing codebase. It infers column sets from
`usecols=` / `columns=` arguments on read calls and propagates them through method chains — no schema
classes required. Add `BaseSchema` classes when you want cross-file awareness and IDE autocomplete.

```python
import pandas as pd

# Checker infers {order_id, amount, status} from usecols= — no annotation needed
orders = pd.read_csv("orders.csv", usecols=["order_id", "amount", "status"])
print(orders["amount"])   # ✓ OK
print(orders["revenue"])  # ✗ unknown-column — 'revenue' not in inferred set
```

## Progressive adoption

**Day one** — run `typedframes check src/` on your existing code. Any `usecols=` / `columns=` calls
you already have give the checker enough to validate downstream column access.

**When it matters** — add `BaseSchema` classes to functions that cross module boundaries. The checker
indexes return type annotations and validates call sites in other files, catching bugs that span the
whole project.

```python
from typing import Annotated
import pandas as pd
from typedframes import BaseSchema, Column


class OrderSchema(BaseSchema):
    order_id = Column(type=int)
    amount = Column(type=float)
    status = Column(type=str)


def load_orders(path: str) -> Annotated[pd.DataFrame, OrderSchema]:
    return pd.read_csv(path, usecols=["order_id", "amount", "status"])
```

Now every file that calls `load_orders()` has its column access validated against `OrderSchema` —
without any annotation in the calling file.

**Across function boundaries** — the checker also infers a *contract* for any function's first
parameter (the columns it needs) and validates it at every call site, even through chains of helper
functions that only forward the parameter along. See [Usage Guide § Function parameter
contracts](usage.md#function-parameter-contracts-missing-column) for the full picture.

## Installation

```shell
pip install typedframes
```

The Rust-based checker is included — no separate install.

## API Reference

Browse the [API Reference](api/index.md) for full documentation of all public classes and functions.

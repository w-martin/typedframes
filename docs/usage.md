# Usage Guide

## Installation

```shell
pip install typedframes
```

For pandas or polars support, install the relevant extra:

```shell
pip install typedframes[pandas]   # includes pandas
pip install typedframes[polars]   # includes polars
```

## Step 1 — Run the checker on existing code

No schema classes required. If your code already uses `usecols=` or `columns=` on read
calls, the checker can validate downstream column access immediately:

```shell
typedframes check src/
```

```python
import pandas as pd

orders = pd.read_csv("orders.csv", usecols=["order_id", "amount", "status"])
print(orders["amount"])   # ✓ OK
print(orders["revenue"])  # ✗ unknown-column — 'revenue' not in inferred set
```

Output uses `file:line:col: severity[code] message` format, matching ty and ruff:

```
src/pipeline.py:42:8: error[unknown-column] Column 'revenue' not in inferred set
```

The checker infers `{order_id, amount, status}` from `usecols=` and propagates that set
through `.rename()`, `.drop()`, `.assign()`, and `.select()` chains.

**Any file format works.** `read_parquet`, `read_json`, `read_excel`, and `read_feather`
are all recognized — just pass `columns=` / `usecols=` to supply column names:

```python
df = pd.read_parquet("orders.parquet", columns=["order_id", "amount"])
pl_df = pl.read_parquet("orders.parquet", columns=["order_id", "amount"])
```

## Step 2 — Add a schema class

Define a `BaseSchema` class when you want cross-file awareness and IDE autocomplete:

```python
from typing import Annotated
import pandas as pd
from typedframes import BaseSchema, Column


class OrderSchema(BaseSchema):
    order_id   = Column(type=int)
    amount     = Column(type=float)
    status     = Column(type=str)


def load_orders(path: str) -> Annotated[pd.DataFrame, OrderSchema]:
    return pd.read_csv(path, usecols=["order_id", "amount", "status"])
```

Now every file that calls `load_orders()` has its column access validated against
`OrderSchema` — even without any annotation in the calling file.

## Step 3 — Use with pandas

Annotate variables with `Annotated[pd.DataFrame, Schema]` and access columns as strings:

```python
from typing import Annotated
import pandas as pd
from typedframes import BaseSchema, Column


class UserSchema(BaseSchema):
    user_id = Column(type=int)
    email   = Column(type=str)
    region  = Column(type=str)


df: Annotated[pd.DataFrame, UserSchema] = pd.read_csv("users.csv")
print(df["user_id"])   # ✓ validated by checker
print(df["username"])  # ✗ unknown-column: 'username' not in UserSchema

# Refactor-safe access via .s descriptor (returns the column name as str)
print(df[UserSchema.user_id.s])
df.groupby(UserSchema.region.s).agg({"amount": "sum"})
```

### Method chains

The checker tracks schema through method chains:

```python
# rename — checker updates the column set
renamed = df.rename(columns={"region": "country"})
print(renamed["country"])  # ✓ OK — renamed
print(renamed["region"])   # ✗ unknown-column — renamed to 'country'

# drop — checker removes the column
slim = df.drop(columns=["region"])
print(slim["user_id"])     # ✓ OK
print(slim["region"])      # ✗ unknown-column — was dropped

# assign — checker adds the new column
enriched = df.assign(domain=df["email"].str.split("@").str[1])
print(enriched["domain"])  # ✓ OK — newly added
```

## Step 4 — Use with polars

The checker validates both subscript access and `pl.col()` references:

```python
from typing import Annotated
import polars as pl
from typedframes import BaseSchema, Column


class EventSchema(BaseSchema):
    event_id  = Column(type=int)
    user_id   = Column(type=int)
    timestamp = Column(type=str)


df: Annotated[pl.DataFrame, EventSchema] = pl.read_csv("events.csv")

# Subscript access — validated
print(df["event_id"])   # ✓ OK
print(df["typo"])        # ✗ unknown-column

# pl.col() references — also validated
df.select(pl.col("event_id"))           # ✓ OK
df.filter(pl.col("typo").is_not_null()) # ✗ unknown-column

# Descriptor .col access — refactor-safe polars expressions
df.filter(EventSchema.user_id.col > 100)
df.select(EventSchema.event_id.col, EventSchema.user_id.col)
```

## Step 5 — Schema composition

Build merged schemas for joins using inheritance or the `+` operator:

```python
from typedframes import BaseSchema, Column, combine_schemas

class OrderSchema(BaseSchema):
    order_id   = Column(type=int)
    amount     = Column(type=float)

class CustomerSchema(BaseSchema):
    customer_id = Column(type=int)
    name        = Column(type=str)

# Multiple inheritance
class ReportSchema(OrderSchema, CustomerSchema):
    region = Column(type=str)

# Or use the + operator
ReportSchema = OrderSchema + CustomerSchema
```

Use `.s` for the merge key:

```python
merged: Annotated[pd.DataFrame, ReportSchema] = orders.merge(
    customers, left_on=OrderSchema.order_id.s, right_on=CustomerSchema.customer_id.s
)
```

## Function parameter contracts (missing-column)

Beyond validating column access at the point it happens, the checker infers a *contract*
for any function's first parameter: every column the function needs, drawn either from
what its body accesses or from a schema annotation on the parameter itself. Calling that
function with a DataFrame that doesn't satisfy the contract is caught at the **call
site** — across files, and through chains of helper functions.

### Inferred from the function body

```python
# transforms.py
def contact_label(customers):
    return customers["name"] + customers["email"]
```

```python
# pipeline.py
from loaders import load_customers
from transforms import contact_label

customers = load_customers(path)  # inferred columns: {customer_id, name, region}
contact_label(customers)
```

```
pipeline.py:5:1: error[missing-column] 'customers' passed to contact_label
  (transforms.py:2) is missing column(s) {email} — available: {customer_id, name,
  region}, required: {email, name}
```

Column-list slices count too — `df[["a", "b"]]` requires both `a` and `b` on the caller.

### Declared via a schema annotation

Annotate the parameter and the schema's full column list becomes the contract, taking
priority over body-scanning. This is more precise: it catches every column the function
needs, not just the ones its body happens to subscript directly.

```python
from typedframes.pandas import PandasFrame

def contact_label(customers: PandasFrame[CustomerSchema]):
    print(customers["name"])
    # 'email' is declared on CustomerSchema but never subscripted here directly —
    # it's still part of the contract, and accessing it inside the function is
    # also validated against CustomerSchema like any other schema-annotated variable.
```

### Transitive through delegate calls

If a function only forwards its own parameter to other functions, the checker follows
the chain and unions their requirements — even when no single function in the chain
touches every required column itself:

```python
def preprocess(df):
    x = df["a"]
    return df

def enrich(df):
    y = df["b"]
    return df

def finalize(df):
    z = df["c"]
    return df

def transform(df):
    step1 = preprocess(df)
    step2 = enrich(step1)
    step3 = finalize(step2)
    return step3
```

`transform` itself never subscripts `df` — but the checker resolves its contract to
`{a, b, c}`, the union of everything `preprocess`, `enrich`, and `finalize` need, so a
caller supplying only `{a, b}` is still flagged at the `transform(df)` call site, even
though `c` is only ever referenced two calls deep, inside `finalize`.

## Exploration mode (untracked-dataframe)

By default, bare DataFrame loads (no `usecols=` / `columns=` / schema annotation) are
silent — the checker has no column information and makes no assumptions. This is
intentional for EDA workflows where you load the full dataset first.

Enable `untracked-dataframe` warnings when you want to enforce that every load has column information:

```shell
typedframes check src/ --strict-ingest
```

Suppress all warnings project-wide via `pyproject.toml`:

```toml
[tool.typedframes]
warnings = false
```

## Pandera integration

Convert a `BaseSchema` to a Pandera schema for runtime value validation:

```python
from typedframes.pandera import to_pandera_schema

pandera_schema = to_pandera_schema(OrderSchema)
validated_df = pandera_schema.validate(pd.read_csv("orders.csv"))
```

typedframes catches **column errors at lint time**; Pandera validates **data values at
runtime**. Use them together for complete coverage.

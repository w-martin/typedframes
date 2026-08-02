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

### Supported column-set transforms

Beyond `rename`/`drop`/`assign`/`select` above, the checker recognizes a fixed,
enumerated set of AST shapes that transform an *already-known* column set. Two are a
case-fold of every column — useful for connectors like Snowflake that genuinely
upper-case unquoted identifiers, when a wrapper function normalizes the case back
before returning:

```python
# .rename(columns=str.lower) / .rename(columns=str.upper) — a callable, not a dict
lowered = df.rename(columns=str.lower)
print(lowered["order_id"])  # ✓ OK — folded
print(lowered["ORDER_ID"])  # ✗ unknown-column — that was the pre-fold spelling

# df.columns = df.columns.str.lower() / .str.upper() — attribute-assignment form
df.columns = df.columns.str.lower()
print(df["order_id"])  # ✓ OK
```

This propagates cross-file the same way any other inferred return schema does: a
helper function that queries a SQL connector and then case-folds the result before
`return`ing it gets its post-fold schema followed at every call site, with no
annotation required. If that helper lives in a genuinely separate, installed package
(a company-internal Snowflake wrapper, say) rather than your own project's source
tree, it isn't indexed by default — see [`trace_external_packages`](api/cli.md#tracing-installed-non-project-packages)
to opt a specific installed package in.

**Only these specific shapes are recognized — not arbitrary transform functions.**
`df.rename(columns=my_company_pkg.normalize_columns)` or any other custom
function/lambda is invisible to the checker: the base schema passes through
*unchanged* (neither folded nor flagged as an error), the same as any other
unrecognized `rename()` argument. This isn't a gap that's merely unimplemented yet —
a static checker can't generally evaluate what an arbitrary Python function does to a
list of strings (that's undecidable in general, per Rice's theorem: the function
could do anything, including data-dependent logic) without actually running it. Only
a finite, explicitly-coded set of well-known patterns can ever be recognized this way;
if your organization's internal SQL wrapper does something not on this list, annotate
its return type explicitly (`Annotated[pd.DataFrame, YourSchema]`) instead of relying
on inference.

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

## Ingestion warnings and exploration mode (untracked-dataframe)

By default, a bare DataFrame load (no `usecols=` / `columns=` / schema annotation)
produces a warning-level `untracked-dataframe` diagnostic — the checker has no column
information for it, and says so.

For EDA workflows where you load the full dataset first and don't want that noise yet,
downgrade it to a quiet info-level note instead:

```shell
typedframes check src/ --lenient-ingest
```

Suppress all warnings project-wide via `pyproject.toml`:

```toml
[tool.typedframes]
warnings = false
```

## Call-site argument tracing (Feast `features=`)

Some functions take their column-determining argument as a *parameter* rather than a
literal — a Feast retrieval wrapper's `features: list[str]`, say — so nothing about the
function's own body can resolve it. typedframes traces a **literal** argument from each
call site back through the parameter, resolving and validating that call independently:

```python
# feast_helpers.py
def load_feature(store, entity_df, feature_names: list[str]):
    df = store.get_historical_features(entity_df=entity_df, features=feature_names).to_df()
    print(df["conv_rate"])  # valid for SOME callers, not others -- see below
```

```python
# pipeline.py
from feast_helpers import load_feature

load_feature(store, entity_df, ["driver_stats:conv_rate"])  # ✓ OK -- resolved cleanly here
load_feature(store, entity_df, ["driver_stats:acc_rate"])   # ✗ unknown-column, reported HERE
```

Both calls run the exact same `print(df["conv_rate"])` line inside `load_feature` — but
each call site is checked **independently**, using whatever literal *that* caller
passed. The diagnostic for the second call is attributed to the call site itself
(`pipeline.py`, not `feast_helpers.py`), which is what makes this possible without one
caller's outcome interfering with another's: `load_feature`'s own body is a single,
caller-independent AST location, so it can only ever be validated once — the
per-caller variation lives entirely in *which literal each call site actually passed*.

A call site passing a non-literal (a variable, a dynamically-built list) doesn't fall
back to a generic warning inside `load_feature` either — it gets its **own**
`untracked-dataframe` warning, attributed to that call site:

```python
dynamic_names = compute_features_somehow()
load_feature(store, entity_df, dynamic_names)  # ⚠ untracked-dataframe, reported HERE
```

The function itself is exactly as resolvable as any other call-site-governed
function — the ambiguity genuinely originates at whichever call site couldn't produce
a literal, not inside a callee whose own shape is perfectly fine in the abstract. The
callee's own generic line is only ever left in place as the sole diagnostic when *no*
call site anywhere in the project is ever traced back to it at all (e.g. the function
is only reached through fully dynamic dispatch) — there the checker still keeps saying
"columns unknown" rather than silently reporting nothing, since there's nowhere else
to put it.

The literal doesn't have to be written out at the call site itself, either:

```python
def get_conv_rate_features():        # zero-arg, returns the literal directly
    return ["driver_stats:conv_rate"]

def get_conv_rate_features_via():    # zero-arg, just forwards to the one above
    return get_conv_rate_features()

load_feature(store, entity_df, get_conv_rate_features_via())  # ✓ OK -- resolved through 2 hops
```

A call site can pass a call to a helper instead of a literal, and that helper's own
`return` is followed — through as many further hops as needed — until a literal is
found (or a cycle, or any other shape this checker declines to guess at, is hit;
recursion is protected against, the same way the existing `requires`/delegate-graph
resolution already guards against a self-referential contract). This isn't limited to
zero-arg forwarding, either — a **literal argument** passed to the helper is
substituted for the helper's own parameter and carried into its return expression:

```python
def get_features(prefix: str):       # takes a real argument
    return [f"{prefix}:conv_rate"]   # builds its return value with an f-string

load_feature(store, entity_df, get_features("driver_stats"))  # ✓ OK -- "driver_stats"
                                                                #    substituted for
                                                                #    `prefix`, f-string
                                                                #    evaluated with it
```

The literal has to actually reach the helper as a literal, though — a call site
passing a *variable* (even one that happens to hold the same string at runtime, like a
value read from an environment variable or config) gives the tracer nothing to
substitute, and the chain stops being traceable there. Only a single positional
argument's worth of substitution is supported per hop (no keyword arguments, no
`*args`/defaults/arity mismatches), the helper's return expression has to be its
*first* `return` (an f-string, a plain string, a list literal built from those, or
another traceable call), and an f-string's interpolations have to be a bare parameter
name with no conversion (`!r`) or format spec (`:>10`). Anything outside that shape —
computing the return value with real logic, multiple statements' worth of
transformation, a helper that takes more than the literal argument itself — isn't
followed, matching this checker's general preference for explicit-shape recognition
over attempting to evaluate arbitrary code.

Deliberately narrow scope otherwise, matching every other heuristic in this checker:
only Feast's chained form (`store.get_historical_features(..., features=<param>).to_df()`)
as a direct statement in the function's own top-level body is recognized, and only call
sites reachable at module level (or nested in `if`/`for`/`while`/`with`, not buried
inside another function) are traced. SQL-text-argument governance (a parameter feeding
`pd.read_sql(<param>, conn)`) isn't covered by this yet.

## SQL and warehouse column inference

Loads from a database or warehouse infer columns from the query's `SELECT` list rather
than a `usecols=`/`columns=` kwarg:

```python
df = pd.read_sql("SELECT order_id, amount FROM orders", conn)
print(df["order_id"])  # OK
print(df["revenue"])   # unknown-column: not in {order_id, amount}
```

The query text is also traced back through a variable assigned exactly once
(`QUERY = "SELECT ..."`, used later — a variable assigned more than once anywhere in
the file is left unresolved, since the checker can't know which assignment was in
effect at the call site) and through a `.sql` file
(`Path("query.sql").read_text()`, project-root-relative only). SQLAlchemy's `text(...)`
and Core `select(Model.col1, Model.col2, ...)` (against a declarative model's columns)
are both recognized too, as are several connector-specific shapes — see
[`docs/api/cli.md`](api/cli.md#sql-and-warehouse-column-inference) for the full list.

An f-string or otherwise dynamically-built query is deliberately left unresolved — the
checker has no taint analysis to tell a safe interpolation from a real SQL-injection
risk, so it falls through to `untracked-dataframe` rather than guessing (or warning
about something it can't actually verify).

Set the target engine's identifier case-folding behavior via `sql_dialect` in
`pyproject.toml` — e.g. Snowflake genuinely upper-cases unquoted identifiers, so
`SELECT order_id FROM orders` really does return a column named `ORDER_ID`, and
`df["order_id"]` against it is a real bug worth catching, not a false positive to
suppress:

```toml
[tool.typedframes]
sql_dialect = "snowflake"
```

Full worked examples for eleven connectors — Snowflake, BigQuery, Athena, Redshift,
Databricks, PySpark, DuckDB, connectorx, SQLAlchemy, Feast, and Azure Synapse/Fabric —
live under `examples/sql_connectors/` in the repo.

## Pandera integration

Convert a `BaseSchema` to a Pandera schema for runtime value validation:

```python
from typedframes.pandera import to_pandera_schema

pandera_schema = to_pandera_schema(OrderSchema)
validated_df = pandera_schema.validate(pd.read_csv("orders.csv"))
```

typedframes catches **column errors at lint time**; Pandera validates **data values at
runtime**. Use them together for complete coverage.

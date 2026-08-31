"""cuDF example (experimental): Annotated[cudf.DataFrame, Schema] on a GPU DataFrame.

RAPIDS cuDF uses pandas' column-access idiom -- plain string subscripts -- so the
schema annotation, the inference from `columns=`/`usecols=`, and the structural
operations (`rename`, `drop`, `assign`, `merge`, ...) all behave exactly as they do
for pandas. No `cudf`-specific spelling is needed anywhere in this file.

cuDF's *reader* surface is narrower than pandas', though, and the checker knows the
difference: `cudf.read_parquet` and `cudf.read_csv` resolve, while `cudf.read_sql`
and `cudf.read_excel` do not, because cuDF exports neither.

This file is checked, never executed. cuDF needs an NVIDIA GPU and a CUDA runtime and
ships Linux-only wheels, but `typedframes check` is pure static analysis over the
source text -- it never imports cudf, so it runs anywhere:

    uv run typedframes check annotated_cudf_example.py

Running `mypy`/`ty` over this file the way the pandas and polars examples do would
need cuDF genuinely installed, which is why only the checker command is listed.
"""

from typing import Annotated

import cudf

from typedframes import BaseSchema, Column


class OrderSchema(BaseSchema):
    """Schema for the orders table."""

    order_id = Column(type=int)
    customer_id = Column(type=int)
    amount = Column(type=float)


class CustomerSchema(BaseSchema):
    """Schema for the customers table."""

    customer_id = Column(type=int)
    region = Column(type=str)


def load_orders() -> Annotated[cudf.DataFrame, OrderSchema]:
    """Read orders onto the GPU and assert OrderSchema."""
    return cudf.read_parquet("orders.parquet", columns=["order_id", "customer_id", "amount"])


def load_customers() -> Annotated[cudf.DataFrame, CustomerSchema]:
    """Read customers onto the GPU and assert CustomerSchema."""
    return cudf.read_parquet("customers.parquet", columns=["customer_id", "region"])


def main() -> None:
    """Demonstrate what the checker catches in cuDF code."""
    orders = load_orders()

    # Valid: every one of these is a real OrderSchema column.
    print(orders["order_id"])
    print(orders["amount"])

    # The cross-file/function contract holds through the return annotation:
    print(orders["revenue"])  # ✗ unknown-column: 'revenue' not in OrderSchema

    # Column sets are inferred from `usecols=` with no schema class at all.
    totals = cudf.read_csv("totals.csv", usecols=["order_id", "total"])
    print(totals["total"])
    print(totals["totl"])  # ✗ unknown-column: did you mean 'total'?

    # Row-preserving cuDF methods pass the column set through unchanged.
    recent = orders.sort_values("amount")
    print(recent["amount"])
    print(recent["timestamp"])  # ✗ unknown-column: 'timestamp' not in OrderSchema

    # Structural operations are tracked: cuDF's rename/drop/assign carry pandas'
    # signatures, so the resulting column set is updated the same way.
    renamed = totals.rename(columns={"total": "gross"})
    print(renamed["gross"])
    print(renamed["total"])  # ✗ unknown-column: renamed away

    enriched = totals.assign(net=0.0)
    print(enriched["net"])

    # Joins combine both sides' schemas. Both operands have to be variables the
    # checker is already tracking -- a merge written directly on call results
    # (`load_orders().merge(load_customers(), ...)`) is not resolved.
    customers = load_customers()
    joined = orders.merge(customers, on="customer_id")
    print(joined["region"])
    print(joined["amount"])
    print(joined["country"])  # ✗ unknown-column: in neither schema

    # `cudf.read_sql` does not exist -- cuDF exports no SQL reader -- so this is left
    # untracked rather than being wrongly resolved from the SELECT list.
    from_sql = cudf.read_sql("SELECT order_id FROM orders", conn)
    print(from_sql["anything"])

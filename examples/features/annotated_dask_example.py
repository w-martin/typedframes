"""Dask example (experimental): column inference and Annotated[dd.DataFrame, Schema].

dask.dataframe is a near-drop-in for pandas, so the checker treats it as a third
backend: `dd.read_csv(...)`/`dd.read_parquet(...)` seed a column set from
`usecols=`/`columns=` exactly as their pandas counterparts do, the structural
operations (drop / rename / assign / subscript slice / del / pop) update it, and the
lazy-to-eager step -- `.compute()`, plus `.persist()`/`.repartition()` -- carries it
through instead of losing it.

Nothing here needs a dask cluster, or even dask itself, to be *checked*: the checker
is a pure AST pass. dask is only imported so this file is a real, runnable program.
"""

from typing import Annotated

import dask.dataframe as dd
import pandas as pd

from typedframes import BaseSchema, Column


class OrderSchema(BaseSchema):
    """Schema for the order records this pipeline reads."""

    order_id = Column(type=int)
    customer_id = Column(type=int)
    amount = Column(type=float)


def load_orders() -> Annotated[dd.DataFrame, OrderSchema]:
    """Load orders lazily, asserting OrderSchema for every caller."""
    return dd.read_csv("orders.csv", usecols=["order_id", "customer_id", "amount"])


def inferred_pipeline() -> None:
    """No schema class at all -- the column set comes from `usecols=` alone."""
    orders = dd.read_csv("orders.csv", usecols=["order_id", "customer_id", "amount"])

    # Row-passthrough and materialization keep the inferred set intact, so the
    # checker is still validating column names after `.compute()`. Each step is
    # bound to its own variable: the checker follows a chain through named
    # intermediates, not through several calls stacked in one expression (that
    # holds for every backend, not just dask).
    recent = orders.query("amount > 0")
    ordered = recent.sort_values("amount")
    totals = ordered.compute()

    print(totals["amount"])
    print(totals["revenue"])  # ✗ unknown-column: 'revenue' not in {order_id, customer_id, amount}

    # Structural operations update the inferred set rather than dropping tracking.
    trimmed = orders.drop(columns=["customer_id"])
    print(trimmed["customer_id"])  # ✗ unknown-column: dropped on the line above

    enriched = orders.assign(tax=1.0)
    print(enriched["tax"])  # ✓ added by assign()


def annotated_pipeline(orders: Annotated[dd.DataFrame, OrderSchema]) -> None:
    """Same validation, driven by an explicit schema instead of inference."""
    print(orders["amount"])
    print(orders["amoutn"])  # ✗ unknown-column: did you mean 'amount'?

    # `.s` gives a refactor-safe string name from the descriptor.
    print(orders[OrderSchema.amount.s])


def from_pandas_pipeline() -> None:
    """`dd.from_pandas` re-wraps an in-memory frame, columns unchanged."""
    pdf = pd.read_csv("orders.csv", usecols=["order_id", "amount"])
    ddf = dd.from_pandas(pdf, npartitions=2)

    print(ddf["amount"])
    print(ddf["customer_id"])  # ✗ unknown-column: the pandas source never had it


def untracked_pipeline() -> None:
    """No `usecols=` and no annotation -- the checker says so instead of guessing."""
    # ⚠ untracked-dataframe: columns unknown at lint time
    anything = dd.read_csv("orders.csv")
    print(anything["whatever"])

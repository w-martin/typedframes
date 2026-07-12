"""Pipeline that consumes typed frames from loaders.py.

This file imports ``load_orders`` and ``load_customers`` from ``loaders.py``.
When you run:

    typedframes check examples/multi_file_with_schema/

the checker builds a project index, resolves each import back to its source
file, and loads the schema associated with each function's return type.  Column
access on ``orders`` and ``customers`` is then validated against ``OrderSchema``
and ``CustomerSchema`` respectively — without any annotation or ``usecols=`` in
this file.

Compare with ``examples/multi_file_inference/`` where each file is checked
independently using only ``usecols=``/``columns=`` inference.  Here, the schema
travels with the function call across the file boundary.

``report_missing_columns`` below demonstrates a third mechanism: a function
*parameter* annotated with a schema (see ``reports.py``) is itself a contract,
checked at every call site — even when, as here, the caller supplies a frame
carrying a different, narrower schema.
"""

from __future__ import annotations

from loaders import load_customers, load_orders
from reports import build_customer_report
from schemas import CustomerSchema, OrderSchema

LARGE_ORDER_AMOUNT = 1000


def summarise_orders(path: str) -> None:
    """Checker validates column access using OrderSchema from the project index."""
    orders = load_orders(path)

    # The checker knows orders has {order_id, customer_id, amount, status}
    # because load_orders is annotated -> Annotated[pd.DataFrame, OrderSchema].

    print(orders["order_id"])  # ✓ OK — in OrderSchema
    print(orders["amount"])  # ✓ OK — in OrderSchema
    # Accessing orders["revenue"] would error: 'revenue' not in OrderSchema
    # Accessing orders["date"] would error: 'date' not in OrderSchema

    # Descriptor .s access — refactor-safe, IDE-autocomplete friendly
    print(orders[OrderSchema.amount.s])  # ✓ OK — descriptor resolves to "amount"
    print(orders[OrderSchema.status.s])  # ✓ OK — descriptor resolves to "status"

    # Method chains on typed frames propagate the schema
    high_value = orders[orders["amount"] > LARGE_ORDER_AMOUNT]  # row filter — schema unchanged
    print(high_value["customer_id"])  # ✓ OK — still OrderSchema columns


def enrich_for_report(orders_path: str) -> None:
    """Combine rename/drop/assign on a schema-typed frame."""
    orders = load_orders(orders_path)

    # rename — checker updates the schema in the inferred set
    normalised = orders.rename(columns={"customer_id": "user_id"})
    print(normalised["user_id"])  # ✓ OK — renamed
    # Accessing normalised["customer_id"] would error: renamed to 'user_id'

    # drop — checker removes the column
    public = normalised.drop(columns=["status"])
    print(public["order_id"])  # ✓ OK
    # Accessing public["status"] would error: 'status' was dropped

    # assign — checker adds the new column
    enriched = public.assign(amount_vat=public["amount"] * 1.2)
    print(enriched["amount_vat"])  # ✓ OK — newly added
    # Accessing enriched["discount"] would error: not in inferred set


def polars_summary(customers_path: str) -> None:
    """Polars pipeline — CustomerSchema travels from loaders.py."""
    customers = load_customers(customers_path)

    # Checker knows customers has {customer_id, name, region}
    print(customers["name"])  # ✓ OK — in CustomerSchema
    print(customers["region"])  # ✓ OK — in CustomerSchema
    # Accessing customers["email"] would error: 'email' not in CustomerSchema

    # Descriptor .s access — refactor-safe, IDE-autocomplete friendly
    print(customers[CustomerSchema.name.s])  # ✓ OK — descriptor resolves to "name"

    # select() narrows the inferred set
    slim = customers.select(["customer_id", "name"])
    print(slim["customer_id"])  # ✓ OK
    # Accessing slim["region"] would error: 'region' not in selected set


def wrong_column_cross_file(path: str) -> None:
    """Accesses a column absent from OrderSchema — caught via the project index.

    ``load_orders`` is in ``loaders.py``, ``OrderSchema`` is in ``schemas.py``.
    The checker traces all three files and reports unknown-column.
    """
    orders = load_orders(path)
    print(orders["revenue"])  # ✗ unknown-column — 'revenue' not in OrderSchema


def report_missing_columns(orders_path: str) -> None:
    """Passes an OrderSchema frame where the full ReportSchema is required.

    ``load_orders`` returns ``Annotated[pd.DataFrame, OrderSchema]`` — only
    {order_id, customer_id, amount, status}. ``build_customer_report``
    (reports.py) requires every ``ReportSchema`` column, since that's how its
    parameter is annotated — it adds {name, region, amount_vat} from the
    customer side of the join. The mismatch is caught via missing-column at
    this call site, even though the function it's missing columns for lives
    in a third file.
    """
    orders = load_orders(orders_path)
    build_customer_report(orders)
    # ✗ missing-column: 'orders' passed to build_customer_report (reports.py:25)
    #   is missing column(s) {amount_vat, name, region}
    #   — available: {amount, customer_id, order_id, status}

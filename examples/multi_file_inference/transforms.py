"""Data transformations that operate on pre-loaded DataFrames.

Functions here accept plain ``pd.DataFrame`` / ``pl.DataFrame`` parameters —
there is no schema attached to a bare parameter type, so the checker cannot
validate column accesses on ``orders`` or ``customers`` *inside* these
functions.

But typedframes still infers a *contract*: every column a function
subscripts directly off its first parameter (``param["col"]``) becomes a
requirement. When pipeline.py calls one of these functions with a variable
whose inferred schema is known, the checker checks that schema against the
function's requirements — and reports a mismatch at the call site, since
that's where the actual columns are known.

``contact_label`` below requires "email", which loaders.py's
``load_customers`` never selects — see pipeline.py for the resulting error.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    import pandas as pd
    import polars as pl

AMOUNT_THRESHOLD = 500


def normalise_orders(orders: pd.DataFrame) -> pd.DataFrame:
    """Rename customer_id → user_id, drop status, add amount_vat."""
    renamed = orders.rename(columns={"customer_id": "user_id"})
    public = renamed.drop(columns=["status"])
    return public.assign(amount_vat=public["amount"] * 1.2)


def slim_for_report(orders: pd.DataFrame) -> pd.DataFrame:
    """Narrow to {order_id, amount} and filter by threshold."""
    slim = orders[["order_id", "amount"]]
    return slim[slim["amount"] > AMOUNT_THRESHOLD]


def contact_label(customers: pl.DataFrame) -> pl.DataFrame:
    """Build a display label from name + email.

    Requires {name, email} on the frame passed in. loaders.py's
    load_customers only selects {customer_id, name, region} — no email —
    so this raises a KeyError at runtime when called from pipeline.py.
    typedframes' function-contract check catches the mismatch at the
    call site instead.
    """
    labels = customers["name"] + " <" + customers["email"] + ">"
    return customers.with_columns(labels.alias("contact_label"))

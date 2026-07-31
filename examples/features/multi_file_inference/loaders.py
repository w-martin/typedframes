"""CSV loaders — aggressive column inference with no schema classes.

The standalone checker infers a column set from ``usecols=`` (pandas) or
``columns=`` (polars) on every read call and validates subscript access against
it.  No ``BaseSchema`` class is required anywhere in this directory.

Run the whole directory at once:

    typedframes check examples/features/multi_file_inference/

Each file is checked independently; the checker reports errors across all of
them in a single pass.
"""

from __future__ import annotations

import pandas as pd
import polars as pl


def load_orders(path: str) -> pd.DataFrame:
    """Load order records.

    The checker sees ``usecols=`` and registers an inferred column set
    {order_id, customer_id, amount, status} for the returned DataFrame.
    Any column access on the result inside *this* function is validated.
    """
    df = pd.read_csv(path, usecols=["order_id", "customer_id", "amount", "status"])

    print(df["order_id"])  # ✓ OK — in usecols
    print(df["amount"])  # ✓ OK — in usecols
    # Accessing df["revenue"] would error: 'revenue' not in {order_id, customer_id, amount, status}
    # Accessing df["date"] would error: 'date' not in {order_id, customer_id, amount, status}

    return df


def load_customers(path: str) -> pl.DataFrame:
    """Load customer records (polars).

    ``columns=`` is the polars equivalent of ``usecols=``; the checker
    treats them identically.
    """
    df = pl.read_csv(path, columns=["customer_id", "name", "region"])

    print(df["customer_id"])  # ✓ OK — in columns
    print(df["region"])  # ✓ OK — in columns
    # Accessing df["email"] would error: 'email' not in {customer_id, name, region}

    return df


def load_wide_table(path: str) -> pd.DataFrame:
    """Load using dtype= dict — checker extracts column names from the keys."""
    df = pd.read_csv(
        path,
        dtype={"product_id": int, "sku": str, "price": float, "stock": int},
    )

    print(df["product_id"])  # ✓ OK — key in dtype dict
    print(df["price"])  # ✓ OK — key in dtype dict
    # Accessing df["description"] would error: 'description' not in inferred column set

    return df


def load_with_wrong_column(path: str) -> pd.DataFrame:
    """Accesses a column that is not in usecols — caught by typedframes, silent in mypy/ty.

    mypy and ty see ``df["revenue"]`` as a valid ``str`` subscript that returns
    ``Any``.  typedframes knows the inferred column set is
    ``{order_id, amount, status}`` and reports unknown-column.
    """
    df = pd.read_csv(path, usecols=["order_id", "amount", "status"])
    print(df["revenue"])  # ✗ unknown-column — 'revenue' not in {order_id, amount, status}
    return df

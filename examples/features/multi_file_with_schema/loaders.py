"""Data loaders that return typed frames.

The return type annotations (``-> Annotated[pd.DataFrame, OrderSchema]``) are
what the checker indexes.  When ``pipeline.py`` calls ``load_orders(path)``,
the checker looks up the function in the project index, resolves its return
type to ``OrderSchema``, and validates all downstream column access against
that schema — without any ``usecols=`` or annotation in ``pipeline.py``.
"""

from __future__ import annotations

from typing import Annotated

import pandas as pd
import polars as pl
from schemas import CustomerSchema, OrderSchema


def load_orders(path: str) -> Annotated[pd.DataFrame, OrderSchema]:
    """Load order records annotated as OrderSchema."""
    return pd.read_csv(path, usecols=["order_id", "customer_id", "amount", "status"])


def load_customers(path: str) -> Annotated[pl.DataFrame, CustomerSchema]:
    """Load customer records annotated as CustomerSchema."""
    return pl.read_csv(path, columns=["customer_id", "name", "region"])

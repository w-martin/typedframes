"""Typedframes and narwhals are complementary: portability vs. type safety.

Narwhals provides backend portability (write once, run on pandas/polars/duckdb).
Typedframes provides static type checking for column names and types.

This example shows that typedframes catches column errors that narwhals misses.
"""

from typing import Annotated

import pandas as pd
import polars as pl

from typedframes import BaseSchema, Column


class OrderSchema(BaseSchema):
    """Schema for order data with typed columns."""

    order_id = Column(type=int)
    customer_name = Column(type=str)
    product_sku = Column(type=str)
    quantity = Column(type=int)
    unit_price = Column(type=float)
    total = Column(type=float)
    shipped = Column(type=bool)


# Typedframes catches column errors via mypy:


def process_pandas(df: Annotated[pd.DataFrame, OrderSchema]) -> pd.Series:
    """Type-checked pandas function.

    With typedframes mypy plugin, this catches the typo:
    - 'custmer_name' should be 'customer_name'

    Args:
        df: A pandas DataFrame that must match OrderSchema.

    Returns:
        A pandas Series (typo: should be 'customer_name').
    """
    # Create local variable with annotation to trigger static checking
    df_checked: Annotated[pd.DataFrame, OrderSchema] = df

    # Mypy catches this typo when using typedframes:
    return df_checked.groupby("custmer_name")["total"].sum()


def process_polars(df: Annotated[pl.DataFrame, OrderSchema]) -> pl.Series:
    """Type-checked polars function.

    With typedframes mypy plugin, this catches the wrong column name:
    - 'sku' should be 'product_sku'

    Args:
        df: A polars DataFrame that must match OrderSchema.

    Returns:
        A polars Series (wrong: should be 'product_sku').
    """
    # Create local variable with annotation to trigger static checking
    df_checked: Annotated[pl.DataFrame, OrderSchema] = df

    # Mypy catches this error when using typedframes:
    return df_checked["sku"]


def process_completely_missing(df: Annotated[pd.DataFrame, OrderSchema]) -> pd.Series:
    """Accessing completely non-existent column.

    Args:
        df: A pandas DataFrame that must match OrderSchema.

    Returns:
        A pandas Series (completely non-existent column).
    """
    # Create local variable with annotation to trigger static checking
    df_checked: Annotated[pd.DataFrame, OrderSchema] = df

    # Mypy catches this error when using typedframes:
    return df_checked["nonexistent"]

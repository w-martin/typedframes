"""Test file to verify typedframes checker catches column typos."""

from typing import Annotated

import pandas as pd

from typedframes import BaseSchema, Column


class OrderSchema(BaseSchema):
    """Schema for orders."""

    order_id = Column(type=int)
    customer_name = Column(type=str)
    total = Column(type=float)


def test_correct_column(df: Annotated[pd.DataFrame, OrderSchema]) -> None:
    """Access correct column."""
    df_annot: Annotated[pd.DataFrame, OrderSchema] = df
    result = df_annot["order_id"]
    print(result)


def test_typo_column(df: Annotated[pd.DataFrame, OrderSchema]) -> None:
    """Access column with typo - should be caught by checker."""
    df_annot: Annotated[pd.DataFrame, OrderSchema] = df
    # TYPO: should be 'customer_name'
    result = df_annot["custmer_name"]
    print(result)


def test_missing_column(df: Annotated[pd.DataFrame, OrderSchema]) -> None:
    """Access nonexistent column - should be caught by checker."""
    df_annot: Annotated[pd.DataFrame, OrderSchema] = df
    # Column doesn't exist in schema
    result = df_annot["nonexistent"]
    print(result)

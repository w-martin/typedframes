"""
typedframes provides static analysis for type-safe column access.

Unlike Great Expectations (runtime validation), typedframes is designed to catch
column typos and wrong column names at lint time, before code runs.

NOTE: The mypy plugin requires careful setup and mypy compatibility. This file
demonstrates the concept and annotation pattern. In production, the plugin
provides static type checking of column access.

Try this:
  uv run mypy c_typedframes_comparison.py --config-file mypy_typedframes.ini 2>&1

With proper plugin setup, output would show static type errors for column access mistakes.
"""

from typing import Annotated

import pandas as pd

from typedframes import BaseSchema, Column


class OrderSchema(BaseSchema):
    """Schema for order data."""

    order_id = Column(type=int)
    customer_name = Column(type=str)
    product_sku = Column(type=str)
    quantity = Column(type=int)
    unit_price = Column(type=float)
    total = Column(type=float)
    shipped = Column(type=bool)


def process_orders(df: Annotated[pd.DataFrame, OrderSchema]) -> pd.Series[str]:
    """Process orders DataFrame.

    Args:
        df: DataFrame with OrderSchema structure.

    Returns:
        Series of customer names.
    """
    # TYPO: should be "customer_name", not "custmer_name"
    # With typedframes plugin, mypy would CATCH THIS
    return df["custmer_name"]  # type: ignore[literal-required]


def get_unit_cost(df: Annotated[pd.DataFrame, OrderSchema]) -> pd.Series[float]:
    """Get unit costs.

    Args:
        df: DataFrame with OrderSchema structure.

    Returns:
        Series of unit costs.
    """
    # WRONG NAME: should be "unit_price", not "unit_cost"
    # With typedframes plugin, mypy would CATCH THIS
    return df["unit_cost"]  # type: ignore[literal-required]


if __name__ == "__main__":
    # Create sample data
    sample_df = pd.DataFrame(
        {
            "order_id": [1, 2, 3],
            "customer_name": ["Alice", "Bob", "Charlie"],
            "product_sku": ["SKU-1", "SKU-2", "SKU-3"],
            "quantity": [2, 1, 5],
            "unit_price": [9.99, 24.99, 15.50],
            "total": [19.98, 24.99, 77.50],
            "shipped": [True, False, True],
        }
    )

    # These calls would fail mypy checking before runtime
    # result = process_orders(sample_df)
    # costs = get_unit_cost(sample_df)

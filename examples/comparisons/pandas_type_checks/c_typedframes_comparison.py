"""
Static analysis comparison: typedframes with mypy plugin.

This demonstrates that typedframes provides STATIC column awareness via mypy plugin.

typedframes catches the same errors BEFORE runtime, at development time.

Run: uv run mypy c_typedframes_comparison.py --config-file mypy_typedframes.ini 2>&1
Expected: mypy REPORTS column access errors statically
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


def process_orders(
    orders: Annotated[pd.DataFrame, OrderSchema],
) -> pd.Series:
    """
    Process orders and return revenue by customer.

    Args:
        orders: DataFrame conforming to OrderSchema

    Returns:
        Series of total revenue by customer
    """
    # Create local variable with annotation to trigger static checking
    orders_checked: Annotated[pd.DataFrame, OrderSchema] = orders

    # ERROR: 'custmer_name' is a typo (should be 'customer_name')
    # ✓ mypy WITH PLUGIN catches this STATICALLY
    # This will be flagged as a type error by mypy before runtime
    return orders_checked.groupby("custmer_name")["total"].sum()


def extract_skus(
    orders: Annotated[pd.DataFrame, OrderSchema],
) -> list[str]:
    """
    Extract product SKUs from orders.

    Args:
        orders: DataFrame conforming to OrderSchema

    Returns:
        List of unique product SKUs
    """
    # Create local variable with annotation to trigger static checking
    orders_checked: Annotated[pd.DataFrame, OrderSchema] = orders

    # ERROR: 'sku' column doesn't exist (should be 'product_sku')
    # ✓ mypy WITH PLUGIN catches this STATICALLY
    skus = orders_checked["sku"].unique()
    return skus.tolist()


def validate_quantities(
    orders: Annotated[pd.DataFrame, OrderSchema],
) -> bool:
    """
    Check that all quantities are positive.

    Args:
        orders: DataFrame conforming to OrderSchema

    Returns:
        True if all quantities are positive
    """
    # Create local variable with annotation to trigger static checking
    orders_checked: Annotated[pd.DataFrame, OrderSchema] = orders

    # ERROR: 'qty' doesn't exist; should be 'quantity'
    # ✓ mypy WITH PLUGIN catches this STATICALLY
    return bool((orders_checked["qty"] > 0).all())


def correct_access(
    orders: Annotated[pd.DataFrame, OrderSchema],
) -> pd.Series:
    """Correct implementation - no errors."""
    # ✓ This is correct and passes static checking
    return orders.groupby("customer_name")["total"].sum()


def main() -> None:
    """Demonstrate the difference."""
    # Create valid sample data
    data = pd.DataFrame(
        {
            "order_id": [1, 2],
            "customer_name": ["Alice", "Bob"],
            "product_sku": ["SKU001", "SKU002"],
            "quantity": [2, 1],
            "unit_price": [10.0, 20.0],
            "total": [20.0, 20.0],
            "shipped": [True, False],
        }
    )

    # At runtime, this would fail; but mypy caught it first
    result = correct_access(data)
    print(result)


if __name__ == "__main__":
    main()

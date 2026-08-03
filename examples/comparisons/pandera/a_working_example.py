"""Idiomatic pandera usage with OrderSchema.

This module demonstrates correct pandera schema usage without type errors.
It defines an OrderSchema and provides functions that work with it correctly.
"""

import pandas as pd
import pandera as pa
from pandera.typing import DataFrame


class OrderSchema(pa.DataFrameModel):
    """Schema for orders DataFrame.

    Columns:
        order_id: Unique order identifier
        customer_name: Customer name
        product_sku: Product SKU
        quantity: Quantity ordered
        unit_price: Price per unit
        total: Total order value
        shipped: Whether order has shipped
    """

    order_id: int
    customer_name: str
    product_sku: str
    quantity: int
    unit_price: float
    total: float
    shipped: bool


def load_orders() -> DataFrame[OrderSchema]:
    """Load sample orders data with OrderSchema.

    Returns:
        DataFrame conforming to OrderSchema with 2 rows of sample data.
    """
    return pd.DataFrame(
        {
            "order_id": [1, 2],
            "customer_name": ["Alice", "Bob"],
            "product_sku": ["SKU-001", "SKU-002"],
            "quantity": [2, 1],
            "unit_price": [9.99, 24.99],
            "total": [19.98, 24.99],
            "shipped": [True, False],
        }
    )


def process_orders(df: DataFrame[OrderSchema]) -> pd.Series:
    """Extract customer names from orders.

    Args:
        df: DataFrame with OrderSchema type annotation.

    Returns:
        Series of customer names (str column).
    """
    # This column access is correct
    return df["customer_name"]


def main() -> None:
    """Load and process orders, print results."""
    orders = load_orders()
    print("Orders DataFrame:")
    print(orders)
    print("\nCustomer names:")
    names = process_orders(orders)
    print(names)


if __name__ == "__main__":
    main()

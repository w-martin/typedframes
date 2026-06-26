"""Narwhals example: cross-backend transformation without type safety.

This demonstrates narwhals' core strength: write once, run on any backend
(pandas, polars, duckdb, etc.). But it does NOT provide type safety for
column names or types.
"""

import narwhals as nw
import pandas as pd
import polars as pl


def compute_high_value_orders(df: nw.DataFrame) -> nw.DataFrame:
    """Works with pandas, polars, or any narwhals-compatible backend.

    Args:
        df: A narwhals DataFrame wrapping any backend (pandas, polars, etc.)

    Returns:
        Filtered DataFrame with only high-value orders (total > 50.0).
    """
    return df.filter(nw.col("total") > 50.0).select(["order_id", "customer_name", "total"])


if __name__ == "__main__":
    # Works with pandas:
    pd_orders = pd.DataFrame(
        {
            "order_id": [1, 2, 3, 4, 5],
            "customer_name": ["Alice", "Bob", "Charlie", "Diana", "Eve"],
            "product_sku": ["SKU-001", "SKU-002", "SKU-003", "SKU-004", "SKU-005"],
            "quantity": [2, 1, 3, 2, 1],
            "unit_price": [9.99, 29.99, 15.50, 24.95, 35.00],
            "total": [99.98, 99.99, 96.50, 99.90, 85.00],
            "shipped": [True, True, False, False, True],
        }
    )
    result_pd = compute_high_value_orders(nw.from_native(pd_orders))
    print("Pandas result:")
    print(result_pd.to_native())
    print()

    # Works with polars:
    pl_orders = pl.DataFrame(
        {
            "order_id": [1, 2, 3, 4, 5],
            "customer_name": ["Alice", "Bob", "Charlie", "Diana", "Eve"],
            "product_sku": ["SKU-001", "SKU-002", "SKU-003", "SKU-004", "SKU-005"],
            "quantity": [2, 1, 3, 2, 1],
            "unit_price": [9.99, 29.99, 15.50, 24.95, 35.00],
            "total": [99.98, 99.99, 96.50, 99.90, 85.00],
            "shipped": [True, True, False, False, True],
        }
    )
    result_pl = compute_high_value_orders(nw.from_native(pl_orders))
    print("Polars result:")
    print(result_pl.to_native())

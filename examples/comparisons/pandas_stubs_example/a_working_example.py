"""
Shows that pandas-stubs enables mypy to understand pandas method signatures.

This file demonstrates what pandas-stubs CAN do: provide type checking for
pandas API methods (DataFrames, Series, groupby, merge, etc.).
"""

import pandas as pd


def main() -> None:
    """Demonstrate pandas-stubs API-level type checking."""
    # pandas-stubs knows that pd.DataFrame() returns a DataFrame
    df: pd.DataFrame = pd.DataFrame(
        {
            "order_id": [1, 2, 3],
            "customer_name": ["Alice", "Bob", "Charlie"],
            "product_sku": ["SKU-1", "SKU-2", "SKU-3"],
            "quantity": [1, 2, 1],
            "unit_price": [9.99, 19.99, 29.99],
            "total": [9.99, 39.98, 29.99],
            "shipped": [True, False, True],
        }
    )

    # pandas-stubs knows the return type of groupby().sum()
    grouped_sum: pd.DataFrame = df.groupby("customer_name")["unit_price"].sum()
    print(f"Grouped sum type: {type(grouped_sum)}")

    # pandas-stubs understands merge() signature and arguments
    other = pd.DataFrame(
        {
            "order_id": [1, 2, 3],
            "region": ["North", "South", "East"],
        }
    )
    merged: pd.DataFrame = df.merge(other, on="order_id")
    print(f"Merged shape: {merged.shape}")

    # pandas-stubs knows that .sum() on Series[float] returns a scalar
    price_series: pd.Series[float] = df["unit_price"]
    total_price: float = price_series.sum()
    print(f"Total price: {total_price}")

    # pandas-stubs catches API errors, e.g., calling .sum() on Series[str]
    name_series: pd.Series[str] = df["customer_name"]
    # Attempting: name_sum = name_series.sum()
    # mypy would warn: error: unsupported operand type(s) for +: "str" and "str"
    # (because Series[str].sum() tries to add strings, which doesn't make sense)
    print(f"Name series dtype: {name_series.dtype}")


if __name__ == "__main__":
    main()

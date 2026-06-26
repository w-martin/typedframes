"""Working dataenforce example — runtime validation of DataFrame schemas.

dataenforce uses a decorator @validate that enforces column names and types
at runtime. The schema is defined inline in the function signature as a
Dataset annotation with column:type pairs.
"""

import pandas as pd
from dataenforce import Dataset, validate


@validate
def process_orders(df: Dataset["order_id":int, "customer_name":str, "unit_price":float]) -> pd.Series:
    """Extract customer names from validated order DataFrame.

    Args:
        df: Orders DataFrame with order_id (int), customer_name (str), unit_price (float)

    Returns:
        Series of customer names
    """
    return df["customer_name"]


@validate
def calculate_total_revenue(df: Dataset["quantity":int, "unit_price":float]) -> float:
    """Calculate total revenue from quantity and unit_price columns.

    Args:
        df: DataFrame with quantity (int) and unit_price (float) columns

    Returns:
        Total revenue as float
    """
    return (df["quantity"] * df["unit_price"]).sum()


if __name__ == "__main__":
    # Valid DataFrame — validation passes at runtime
    orders = pd.DataFrame(
        {
            "order_id": [1, 2, 3],
            "customer_name": ["Alice", "Bob", "Carol"],
            "unit_price": [19.99, 29.50, 15.75],
        }
    )

    print("Valid DataFrame:")
    print(orders)
    print("\nExtracted customer names:")
    result = process_orders(orders)
    print(result)
    print()

    # Calculate revenue
    revenue = calculate_total_revenue(
        pd.DataFrame(
            {
                "quantity": [2, 1, 3],
                "unit_price": [19.99, 29.50, 15.75],
            }
        )
    )
    print(f"Total revenue: ${revenue:.2f}")
    print()

    # Invalid DataFrame — validation fails at runtime
    print("Invalid DataFrame (missing unit_price column):")
    try:
        invalid_orders = pd.DataFrame(
            {
                "order_id": [1, 2, 3],
                "customer_name": ["Alice", "Bob", "Carol"],
            }
        )
        process_orders(invalid_orders)
    except Exception as e:
        print(f"Runtime error caught: {type(e).__name__}: {e}")

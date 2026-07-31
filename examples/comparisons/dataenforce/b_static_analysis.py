"""Static analysis of dataenforce code — mypy provides no schema validation.

Despite the Dataset annotation in the function signature, mypy cannot understand
the column schema syntax (Dataset["col":type, ...]). The decorator is purely
runtime validation, so column typos and schema mismatches are not caught by
static analysis.

This file demonstrates:
1. Accessing a non-existent column (typo: "custmer_name" instead of "customer_name")
   — mypy is SILENT (no error)
2. Calling the function with mismatched data — mypy is SILENT (error only happens at runtime)
"""

import pandas as pd
from dataenforce import Dataset, validate


@validate
def process_orders(df: Dataset["order_id":int, "customer_name":str, "unit_price":float]) -> pd.Series:
    """Extract customer names from order DataFrame.

    Args:
        df: Orders DataFrame with order_id (int), customer_name (str), unit_price (float)

    Returns:
        Series of customer names
    """
    # BUG: Typo in column name "custmer_name" (missing 's')
    # mypy does NOT catch this — it's runtime-only checking
    return df["custmer_name"]


@validate
def calculate_revenue(df: Dataset["quantity":int, "unit_price":float]) -> float:
    """Calculate total revenue.

    Args:
        df: DataFrame with quantity (int) and unit_price (float)

    Returns:
        Total revenue as float
    """
    # BUG: Accessing "quanity" (typo: missing 't')
    # mypy does NOT catch this
    return (df["quanity"] * df["unit_price"]).sum()


# This function call will pass mypy even with wrong data
def main() -> None:
    """Call functions with incompatible data."""
    # This DataFrame is missing the "unit_price" column,
    # but mypy is SILENT — error only occurs at runtime
    invalid_df = pd.DataFrame(
        {
            "order_id": [1, 2, 3],
            "customer_name": ["Alice", "Bob", "Carol"],
        }
    )

    # mypy won't complain about calling process_orders with incomplete schema
    _ = process_orders(invalid_df)

    # This will also pass mypy but fail at runtime
    another_invalid = pd.DataFrame(
        {
            "quantity": [1, 2, 3],
            # Missing "unit_price" column
        }
    )
    _ = calculate_revenue(another_invalid)


if __name__ == "__main__":
    main()

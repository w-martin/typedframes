"""
Working example of pandas-type-checks runtime validation.

pandas-type-checks is a runtime-only library that validates DataFrame and Series
parameters at function call time. It does NOT provide static type checking via mypy.

Key characteristics:
- Decorator-based runtime validation
- Validates column names and dtypes at call time
- Strict mode prevents extra columns; non-strict allows structural subtyping
- Works with pandas DataFrames and Series
"""

import pandas as pd
from pandas_type_checks import DataFrameArgument, pandas_type_check

# Define type specifications using DataFrameArgument
order_schema = DataFrameArgument(
    name="orders",
    dtype={
        "order_id": "int64",
        "customer_name": "object",
        "product_sku": "object",
        "quantity": "int64",
        "unit_price": "float64",
        "total": "float64",
        "shipped": "bool",
    },
)


@pandas_type_check(order_schema)
def process_orders(orders: pd.DataFrame) -> pd.Series:
    """
    Process orders and compute revenue by customer.

    Args:
        orders: DataFrame with order details

    Returns:
        Series of total revenue by customer
    """
    return orders.groupby("customer_name")["total"].sum()


def main() -> None:
    # Create sample data matching the schema
    valid_data = pd.DataFrame(
        {
            "order_id": [1, 2, 3, 4],
            "customer_name": ["Alice", "Bob", "Alice", "Charlie"],
            "product_sku": ["SKU001", "SKU002", "SKU001", "SKU003"],
            "quantity": [2, 1, 3, 1],
            "unit_price": [10.0, 20.0, 10.0, 15.0],
            "total": [20.0, 20.0, 30.0, 15.0],
            "shipped": [True, False, True, True],
        }
    )

    print("=== Valid data (passes runtime check) ===\n")
    print(valid_data)
    print("\n=== Processing result ===\n")
    result = process_orders(valid_data)
    print(result)

    # Try with wrong dtype
    print("\n\n=== Invalid data (wrong dtype) ===\n")
    invalid_dtype = valid_data.copy()
    invalid_dtype["quantity"] = invalid_dtype["quantity"].astype("float64")
    print(invalid_dtype.dtypes)

    try:
        process_orders(invalid_dtype)
    except TypeError as e:
        print(f"\nRuntime error caught: {type(e).__name__}")
        print(f"Message snippet: {str(e)[:150]}...")

    # Try with missing column
    print("\n\n=== Invalid data (missing column) ===\n")
    missing_col = valid_data.drop(columns=["shipped"])
    print(f"Columns: {list(missing_col.columns)}")

    try:
        process_orders(missing_col)
    except TypeError as e:
        print(f"\nRuntime error caught: {type(e).__name__}")
        print(f"Message snippet: {str(e)[:150]}...")

    # Try with extra column (non-strict mode allows this)
    print("\n\n=== Extra columns (allowed in non-strict mode) ===\n")
    extra_col = valid_data.copy()
    extra_col["notes"] = "extra"
    print(f"Columns: {list(extra_col.columns)}")

    result = process_orders(extra_col)
    print("Success - extra columns allowed in non-strict mode")


if __name__ == "__main__":
    main()

"""
Static analysis test: mypy with pandas-type-checks (NO PLUGIN).

This demonstrates that pandas-type-checks provides NO static type checking.
mypy sees no column information—it cannot detect:

1. Accessing non-existent column names
2. Type incompatibilities between column operations
3. Schema violations

The decorator specification is purely runtime metadata; mypy treats
decorated functions as black boxes with standard pandas types.

Run: uv run mypy b_static_analysis.py --config-file mypy.ini 2>&1
Expected: mypy is SILENT on column access errors
"""

import pandas as pd
from pandas_type_checks import DataFrameArgument, pandas_type_check

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
    """Process orders and return revenue by customer."""
    # ERROR: 'custmer_name' is a typo (should be 'customer_name')
    # pandas-type-checks catches this at RUNTIME only
    # mypy sees NO ERROR because it has no column awareness
    return orders.groupby("custmer_name")["total"].sum()


@pandas_type_check(order_schema)
def extract_skus(orders: pd.DataFrame) -> list[str]:
    """Extract product SKUs from orders."""
    # ERROR: 'sku' column doesn't exist (should be 'product_sku')
    # This is silently missed by mypy without the plugin
    skus = orders["sku"].unique()
    return skus.tolist()


@pandas_type_check(order_schema)
def validate_quantities(orders: pd.DataFrame) -> bool:
    """Check that all quantities are positive."""
    # ERROR: 'qty' doesn't exist; should be 'quantity'
    # mypy will NOT flag this as an error
    return bool((orders["qty"] > 0).all())


def main() -> None:
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

    # These will fail at RUNTIME, but mypy has no idea
    try:
        process_orders(data)
    except KeyError as e:
        print(f"Runtime KeyError in process_orders: {e}")

    try:
        extract_skus(data)
    except KeyError as e:
        print(f"Runtime KeyError in extract_skus: {e}")

    try:
        validate_quantities(data)
    except KeyError as e:
        print(f"Runtime KeyError in validate_quantities: {e}")


if __name__ == "__main__":
    main()

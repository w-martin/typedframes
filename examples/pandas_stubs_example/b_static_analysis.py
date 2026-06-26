"""
Shows what pandas-stubs catches and misses regarding column access.

pandas-stubs provides type stubs for pandas methods, but it has ZERO
awareness of which columns exist in a DataFrame. Column names are
arbitrary strings at runtime, so pandas-stubs cannot know whether
df["custmer_name"] (typo) is valid or invalid.

Run:
    uv run mypy b_static_analysis.py --config-file mypy.ini
"""

import pandas as pd


def main() -> None:
    """Demonstrate pandas-stubs column access limitations."""
    # Create a DataFrame with specific columns
    df: pd.DataFrame = pd.DataFrame(
        {
            "order_id": [1, 2],
            "customer_name": ["Alice", "Bob"],
            "product_sku": ["SKU-1", "SKU-2"],
            "quantity": [1, 2],
            "unit_price": [9.99, 19.99],
            "total": [9.99, 39.98],
            "shipped": [True, False],
        }
    )

    # Valid column access — what type does mypy infer?
    x = df["customer_name"]
    print(f"x type: {type(x)}, dtype: {x.dtype}")

    # TYPO: "custmer_name" instead of "customer_name"
    # Does pandas-stubs catch this? NO
    y = df["custmer_name"]
    print(f"y type: {type(y)}, dtype: {y.dtype}")

    # WRONG NAME: "unit_cost" doesn't exist (should be "unit_price")
    # Does pandas-stubs catch this? NO
    z = df["unit_cost"]
    print(f"z type: {type(z)}, dtype: {z.dtype}")

    # NONEXISTENT COLUMN: completely made up name
    # Does pandas-stubs catch this? NO
    w = df["completely_made_up"]
    print(f"w type: {type(w)}, dtype: {w.dtype}")


if __name__ == "__main__":
    main()

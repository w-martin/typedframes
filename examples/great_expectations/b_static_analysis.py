"""
Great Expectations does NOT provide static analysis for column access.

GX validates data quality at runtime (values, distributions, nullability),
but mypy cannot know about your column names just by looking at the code.

Try this:
  uv run mypy b_static_analysis.py --config-file mypy.ini 2>&1

Expected output: No errors (mypy has no way to know the DataFrame schema).
"""

import pandas as pd


def main() -> None:
    df = pd.DataFrame(
        {
            "order_id": [1, 2],
            "customer_name": ["Alice", "Bob"],
            "product_sku": ["SKU-1", "SKU-2"],
            "quantity": [2, 1],
            "unit_price": [9.99, 24.99],
            "total": [19.98, 24.99],
            "shipped": [True, False],
        }
    )

    # TYPO: should be "customer_name", not "custmer_name"
    # mypy will NOT catch this — it only sees df[...] which returns Any
    x: pd.Series = df["custmer_name"]  # type: ignore[assignment]

    # WRONG NAME: should be one of the actual columns
    # mypy will NOT catch this either
    y: pd.Series = df["unit_cost"]  # type: ignore[assignment]

    print(f"x shape: {x.shape}")  # Would fail at runtime with KeyError
    print(f"y shape: {y.shape}")  # Would fail at runtime with KeyError


if __name__ == "__main__":
    main()

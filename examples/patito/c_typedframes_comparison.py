"""
Demonstrating how typedframes catches the same errors patito misses.

typedframes uses mypy plugins to provide static type checking for
DataFrame column access. The same typos that patito misses are caught
at lint-time with typedframes.

Run:
    python -m mypy c_typedframes_comparison.py --config-file mypy_typedframes.ini
"""

from typing import Annotated

import polars as pl

from typedframes import BaseSchema, Column


class OrderSchema(BaseSchema):
    """Order schema defined with typedframes."""

    order_id = Column(type=int)
    customer_name = Column(type=str)
    product_sku = Column(type=str)
    quantity = Column(type=int)
    unit_price = Column(type=float)
    total = Column(type=float)
    shipped = Column(type=bool)


def main() -> None:
    """Demonstrate typedframes column-level type checking."""
    # Annotate the DataFrame with the schema
    df: Annotated[pl.DataFrame, OrderSchema] = pl.DataFrame(
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

    # Valid column access — mypy knows the type
    x = df["customer_name"]
    print(f"x: {x.to_list()}")

    # TYPO: "custmer_name" instead of "customer_name"
    # typedframes plugin catches this and suggests: did you mean 'customer_name'?
    y = df["custmer_name"]  # Error: Column does not exist
    print(f"y: {y.to_list()}")

    # WRONG NAME: "unit_cost" doesn't exist (should be "unit_price")
    # typedframes plugin catches this and suggests: did you mean 'unit_price'?
    z = df["unit_cost"]  # Error: Column does not exist
    print(f"z: {z.to_list()}")

    # NONEXISTENT COLUMN: completely made up name
    # typedframes plugin catches this
    w = df["completely_made_up"]  # Error: Column does not exist
    print(f"w: {w.to_list()}")


if __name__ == "__main__":
    main()

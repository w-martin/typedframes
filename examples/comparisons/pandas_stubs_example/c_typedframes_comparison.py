"""
Same column access bugs as b_static_analysis.py, but with typedframes.

With typedframes' mypy plugin, we define a schema that describes the
DataFrame's structure. The plugin then type-checks column access against
the schema, catching typos and nonexistent columns at static analysis time.

Run:
    uv run mypy c_typedframes_comparison.py --config-file mypy_typedframes.ini
"""

from typing import Annotated

import pandas as pd

from typedframes import BaseSchema, Column


class OrderSchema(BaseSchema):
    """Schema describing the structure of an Orders DataFrame."""

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
    df: Annotated[pd.DataFrame, OrderSchema] = pd.DataFrame(
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
    print(f"x: {x.tolist()}")

    # TYPO: "custmer_name" instead of "customer_name"
    # typedframes plugin catches this and suggests: did you mean 'customer_name'?
    y = df["custmer_name"]
    print(f"y: {y.tolist()}")

    # WRONG NAME: "unit_cost" doesn't exist (should be "unit_price")
    # typedframes plugin catches this and suggests: did you mean 'unit_price'?
    z = df["unit_cost"]
    print(f"z: {z.tolist()}")

    # NONEXISTENT COLUMN: completely made up name
    # typedframes plugin catches this: "unknown-column"
    w = df["completely_made_up"]
    print(f"w: {w.tolist()}")


if __name__ == "__main__":
    main()

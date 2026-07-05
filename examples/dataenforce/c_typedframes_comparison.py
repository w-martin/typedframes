"""Static analysis comparison with typedframes — catches schema errors at lint time.

typedframes uses BaseSchema with Column definitions and Annotated type hints.
The mypy plugin validates column access and data flow at lint-time, catching
typos and schema mismatches before the code runs.

This file uses the same column access patterns as b_static_analysis.py but
with typedframes — note how mypy catches the errors.

Run:
    uv run mypy c_typedframes_comparison.py --config-file mypy_typedframes.ini
"""

from typing import Annotated

import pandas as pd

from typedframes import BaseSchema, Column


class OrderSchema(BaseSchema):
    """Order DataFrame schema."""

    order_id = Column(type=int)
    customer_name = Column(type=str)
    unit_price = Column(type=float)


class RevenueSchema(BaseSchema):
    """Revenue calculation schema."""

    quantity = Column(type=int)
    unit_price = Column(type=float)


def main() -> None:
    """Demonstrate typedframes column-level type checking."""
    # Annotate the DataFrame with the schema
    orders: Annotated[pd.DataFrame, OrderSchema] = pd.DataFrame(
        {
            "order_id": [1, 2, 3],
            "customer_name": ["Alice", "Bob", "Carol"],
            "unit_price": [19.99, 29.50, 15.75],
        }
    )

    # Valid column access — mypy knows the type
    valid = orders["customer_name"]
    print(f"Valid access: {valid.tolist()}")

    # BUG: Typo in column name "custmer_name" (missing 's')
    # typedframes plugin CATCHES THIS and suggests: did you mean 'customer_name'?
    typo = orders["custmer_name"]
    print(f"Typo access: {typo.tolist()}")

    # BUG: Wrong column name "unit_cost" (should be "unit_price")
    # typedframes plugin CATCHES THIS and suggests: did you mean 'unit_price'?
    wrong = orders["unit_cost"]
    print(f"Wrong access: {wrong.tolist()}")

    # Revenue calculation with schema
    revenue_data: Annotated[pd.DataFrame, RevenueSchema] = pd.DataFrame(
        {
            "quantity": [2, 1, 3],
            "unit_price": [19.99, 29.50, 15.75],
        }
    )

    # Valid calculation
    total = (revenue_data["quantity"] * revenue_data["unit_price"]).sum()
    print(f"Revenue: {total}")

    # BUG: Typo "quanity" (missing 't')
    # typedframes plugin CATCHES THIS
    bad_total = (revenue_data["quanity"] * revenue_data["unit_price"]).sum()
    print(f"Bad revenue: {bad_total}")


if __name__ == "__main__":
    main()

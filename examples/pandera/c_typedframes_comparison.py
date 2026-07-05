"""Typedframes schema validation: Catching column name errors at lint time.

This module demonstrates typedframes' ability to catch column name errors
using static analysis. Unlike pandera, typedframes catches:
  - Column name typos (df["custmer_name"] vs df["customer_name"])
  - Non-existent column access (df["unit_cost"] vs df["unit_price"])

These errors are caught by the typedframes.mypy plugin at type-check time,
not just at runtime.

Expected mypy output (typedframes.mypy plugin):
  c_typedframes_comparison.py:85: error: Unknown column "custmer_name"
    (did you mean "customer_name"?) [call-arg]
  c_typedframes_comparison.py:88: error: Unknown column "unit_cost"
    (did you mean "unit_price"?) [call-arg]
"""

from typing import Annotated

import pandas as pd

from typedframes import BaseSchema, Column


class OrderSchema(BaseSchema):
    """Schema for orders DataFrame.

    Columns:
        order_id: Unique order identifier
        customer_name: Customer name
        product_sku: Product SKU
        quantity: Quantity ordered
        unit_price: Price per unit
        total: Total order value
        shipped: Whether order has shipped
    """

    order_id = Column(type=int)
    customer_name = Column(type=str)
    product_sku = Column(type=str)
    quantity = Column(type=int)
    unit_price = Column(type=float)
    total = Column(type=float)
    shipped = Column(type=bool)


def main() -> None:
    """Demonstrate typedframes catching column name errors at lint time."""
    # Annotate the DataFrame with the schema
    orders: Annotated[pd.DataFrame, OrderSchema] = pd.DataFrame(
        {
            "order_id": [1, 2],
            "customer_name": ["Alice", "Bob"],
            "product_sku": ["SKU-001", "SKU-002"],
            "quantity": [2, 1],
            "unit_price": [9.99, 24.99],
            "total": [19.98, 24.99],
            "shipped": [True, False],
        }
    )
    print("Orders DataFrame:")
    print(orders)

    # CAUGHT: Typo in column name (custmer_name instead of customer_name)
    # typedframes' mypy plugin CATCHES this error with "Unknown column" message
    print(orders["custmer_name"])  # <- typedframes CATCHES this, suggests "customer_name"

    # CAUGHT: Non-existent column name (unit_cost instead of unit_price)
    # typedframes' mypy plugin CATCHES this error with "Unknown column" message
    print(orders["unit_cost"])  # <- typedframes CATCHES this, suggests "unit_price"


# ============================================================================
# ACTUAL MYPY OUTPUT (typedframes.mypy plugin):
# ============================================================================
# $ uv run mypy c_typedframes_comparison.py --config-file mypy_typedframes.ini
#
# c_typedframes_comparison.py:65: error: Column 'custmer_name' does not exist
#   in OrderSchema (defined at line 51) (did you mean 'customer_name'?)
#   [misc]
# c_typedframes_comparison.py:69: error: Column 'unit_cost' does not exist
#   in OrderSchema (defined at line 51)  [misc]
# Found 2 errors in 1 file (checked 1 source file)
#
# NOTE: Typedframes catches both column name errors at type-check time,
# providing helpful suggestions via Levenshtein distance matching.
# ============================================================================

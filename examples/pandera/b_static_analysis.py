"""Pandera mypy plugin: What it catches and what it misses.

This module demonstrates the limitations of pandera's mypy plugin:
  - CATCHES: Wrong schema type passed to function (type annotation mismatch)
  - MISSES: Column name typos (df["custmer_name"] vs df["customer_name"])
  - MISSES: Non-existent column access (df["unit_cost"] vs df["unit_price"])

Expected mypy output (pandera.mypy plugin):
  b_static_analysis.py:52: error: Argument 1 to "process_orders" has
    incompatible type "DataFrame[WrongSchema]"; expected "DataFrame[OrderSchema]"
    [arg-type]

Column name errors (lines 53-54) are NOT caught by pandera's mypy plugin.

Note: typedframes' *standalone* checker (`typedframes check`, no mypy involved)
recognizes pandera's ``DataFrameModel``/``DataFrame[Schema]`` pattern natively
and independently catches the wrong-schema-argument case below too, via
missing-column — see ``main()``.
"""

import pandas as pd
import pandera as pa
from pandera.typing import DataFrame


class OrderSchema(pa.DataFrameModel):
    """Schema for orders DataFrame."""

    order_id: int
    customer_name: str
    product_sku: str
    quantity: int
    unit_price: float
    total: float
    shipped: bool


class WrongSchema(pa.DataFrameModel):
    """A different schema, used to demonstrate type checking."""

    id: int
    name: str


def load_orders() -> DataFrame[OrderSchema]:
    """Load sample orders data with OrderSchema."""
    df = pd.DataFrame(
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
    return OrderSchema.validate(df)


def load_wrong_schema() -> DataFrame[WrongSchema]:
    """Load data with wrong schema type."""
    df = pd.DataFrame(
        {
            "id": [1, 2],
            "name": ["X", "Y"],
        }
    )
    return WrongSchema.validate(df)


def process_orders(df: DataFrame[OrderSchema]) -> pd.Series:
    """Extract customer names from orders.

    Args:
        df: DataFrame with OrderSchema type annotation.

    Returns:
        Series of customer names.
    """
    return df["customer_name"]


def main() -> None:
    """Demonstrate pandera mypy plugin catching and missing errors."""
    orders = load_orders()
    wrong = load_wrong_schema()

    # CAUGHT: Passing WrongSchema to function expecting OrderSchema
    # Expected error: "Argument 1 to "process_orders" has incompatible type"
    # pandera's mypy plugin catches this as a type mismatch. typedframes'
    # standalone checker independently catches it too, via missing-column —
    # no mypy required, since it recognizes DataFrameModel-annotated params.
    process_orders(wrong)

    # MISSED: Typo in column name (custmer_name instead of customer_name)
    # pandera's mypy plugin does NOT catch this error
    print(orders["custmer_name"])  # <- NOT caught, no error from pandera

    # MISSED: Non-existent column name (unit_cost instead of unit_price)
    # pandera's mypy plugin does NOT catch this error
    print(orders["unit_cost"])  # <- NOT caught, no error from pandera


# ============================================================================
# ACTUAL MYPY OUTPUT (pandera.mypy plugin):
# ============================================================================
# $ uv run mypy b_static_analysis.py --config-file mypy.ini
#
# b_static_analysis.py:82: error: Argument 1 to "process_orders" has
#   incompatible type "DataFrame[WrongSchema]"; expected "DataFrame[OrderSchema]"
#   [arg-type]
# Found 1 error in 1 file (checked 1 source file)
#
# NOTE: The typo at line 85 (custmer_name) and wrong column at line 89
# (unit_cost) are NOT caught by pandera's mypy plugin. These errors would
# only be caught at runtime when the code runs and KeyError is raised.
# ============================================================================

"""
Comparison with typedframes: same bugs, different error detection.

This file uses typedframes with the same data model and intentional bugs
to show how it differs from strictly-typed-pandas static analysis.
"""

from typing import Annotated

import pandas as pd
import polars as pl

from typedframes import BaseSchema, Column


class OrderSchema(BaseSchema):
    """Schema for orders using typedframes."""

    order_id = Column(type=int)
    customer_name = Column(type=str)
    product_sku = Column(type=str)
    quantity = Column(type=int)
    unit_price = Column(type=float)
    total = Column(type=float)
    shipped = Column(type=bool)


def load_orders_pandas() -> Annotated[pd.DataFrame, OrderSchema]:
    """Load orders as pandas DataFrame with schema annotation."""
    return pd.DataFrame(
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


def load_orders_polars() -> Annotated[pl.DataFrame, OrderSchema]:
    """Load orders as polars DataFrame with schema annotation."""
    return pl.DataFrame(
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


def process_pandas(orders: Annotated[pd.DataFrame, OrderSchema]) -> None:
    """Process pandas orders with intentional column name errors."""
    # Create a local variable with the schema annotation to trigger checking
    orders_annot: Annotated[pd.DataFrame, OrderSchema] = orders

    # Bug 1: Typo in column name — "custmer_name" instead of "customer_name"
    # typedframes should catch this
    custmer_series = orders_annot["custmer_name"]
    print(f"Custmer series: {custmer_series}")

    # Bug 2: Wrong column name — "unit_cost" instead of "unit_price"
    # typedframes should catch this
    cost_series = orders_annot["unit_cost"]
    print(f"Cost series: {cost_series}")


def process_polars(orders: Annotated[pl.DataFrame, OrderSchema]) -> None:
    """Process polars orders with the same errors.

    Note: strictly-typed-pandas has no polars support. This demonstrates
    typedframes' multi-dataframe support.
    """
    # Create a local variable with the schema annotation to trigger checking
    orders_annot: Annotated[pl.DataFrame, OrderSchema] = orders

    # Same bugs on polars DataFrame
    custmer_series = orders_annot["custmer_name"]
    print(f"Custmer series (polars): {custmer_series}")

    cost_series = orders_annot["unit_cost"]
    print(f"Cost series (polars): {cost_series}")


def schema_mismatch_test() -> None:
    """Test schema mismatch detection."""

    class DifferentSchema(BaseSchema):
        """Different schema."""

        other_id = Column(type=int)
        other_name = Column(type=str)

    wrong_orders: Annotated[pd.DataFrame, DifferentSchema] = pd.DataFrame(
        {
            "other_id": [1],
            "other_name": ["test"],
        }
    )

    # This should error: DifferentSchema != OrderSchema when passed to process_pandas
    # Note: The function parameter type annotation doesn't trigger Rust checking,
    # only local variable assignments do.
    test_with_order_schema: Annotated[pd.DataFrame, OrderSchema] = wrong_orders  # noqa: F841


def main() -> None:
    """Run comparison tests."""
    print("=== Pandas Orders ===")
    orders_pd = load_orders_pandas()
    process_pandas(orders_pd)

    print("\n=== Polars Orders (not supported by strictly-typed-pandas) ===")
    orders_pl = load_orders_polars()
    process_polars(orders_pl)

    print("\n=== Schema Mismatch Test ===")
    schema_mismatch_test()


if __name__ == "__main__":
    main()


# ==============================================================================
# MYPY OUTPUT WITH typedframes.mypy
# ==============================================================================
#
# $ uv run mypy c_typedframes_comparison.py --config-file mypy_typedframes.ini
#
# c_typedframes_comparison.py:64: error: Column 'custmer_name' does not exist in
#   OrderSchema (defined at line 60) (did you mean 'customer_name'?)  [misc]
# c_typedframes_comparison.py:69: error: Column 'unit_cost' does not exist in
#   OrderSchema (defined at line 60)  [misc]
# c_typedframes_comparison.py:83: error: Column 'custmer_name' does not exist in
#   OrderSchema (defined at line 80) (did you mean 'customer_name'?)  [misc]
# c_typedframes_comparison.py:86: error: Column 'unit_cost' does not exist in
#   OrderSchema (defined at line 80)  [misc]
# Found 4 errors in 1 file (checked 1 source file)
#
# KEY FINDINGS:
# - typedframes WITH mypy plugin catches all column name typos
# - Provides helpful "did you mean?" suggestions for typos
# - Works for both pandas and polars DataFrames (different from stp)
# - Column checking is context-aware: only triggers on annotated variables
# - Rust-based checker is very fast (builds project index once, caches results)
#

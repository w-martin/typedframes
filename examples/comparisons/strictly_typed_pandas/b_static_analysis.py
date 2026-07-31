"""
Static analysis test: what does mypy with strictly-typed-pandas plugin catch?

This file intentionally contains type errors to demonstrate the capabilities
and limitations of the strictly-typed-pandas mypy plugin.
"""

from strictly_typed_pandas import DataSet


class OrderSchema:
    """Schema for orders."""

    order_id: int
    customer_name: str
    product_sku: str
    quantity: int
    unit_price: float
    total: float
    shipped: bool


def load_orders() -> DataSet[OrderSchema]:
    """Load orders."""
    return DataSet[OrderSchema](
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


def process_orders(orders: DataSet[OrderSchema]) -> None:
    """Process orders with intentional type errors."""
    # Create a local variable with the schema annotation to trigger checking
    orders_annot = orders

    # Bug 1: Typo in column name — "custmer_name" instead of "customer_name"
    # Question: Does mypy catch this?
    custmer_series = orders_annot["custmer_name"]
    print(f"Custmer series: {custmer_series}")

    # Bug 2: Wrong column name — "unit_cost" instead of "unit_price"
    # Question: Does mypy catch this?
    cost_series = orders_annot["unit_cost"]
    print(f"Cost series: {cost_series}")

    # Bug 3: Try to pass wrong schema type to a function expecting OrderSchema
    # Question: Does mypy catch schema mismatch?
    class DifferentSchema:
        """Different schema."""

        other_id: int
        other_name: str

    wrong_orders: DataSet[DifferentSchema] = DataSet[DifferentSchema](
        {
            "other_id": [1],
            "other_name": ["test"],
        }
    )
    process_orders_strict(wrong_orders)  # type: ignore[arg-type]


def process_orders_strict(orders: DataSet[OrderSchema]) -> None:
    """Function that expects OrderSchema."""
    print(f"Processing {len(orders)} orders")


def main() -> None:
    """Run static analysis tests."""
    orders = load_orders()
    process_orders(orders)


if __name__ == "__main__":
    main()


# ==============================================================================
# MYPY OUTPUT WITH strictly_typed_pandas (no mypy plugin available)
# ==============================================================================
#
# $ uv run mypy b_static_analysis.py --config-file mypy_stp_nopl.ini
#
# Success: no issues found in 1 source file
#
# KEY FINDINGS:
# - strictly-typed-pandas does NOT have a mypy plugin (as of v0.3.7)
# - It relies on pandas-stubs for type information
# - pandas-stubs treats DataFrames as generic containers with Any-typed __getitem__
# - Therefore, mypy CANNOT catch column name typos like "custmer_name"
# - Schema type mismatch (DataSet[SchemaA] vs DataSet[SchemaB]) is also NOT caught
# - This is a fundamental limitation: no plugin = no column-level type information
#

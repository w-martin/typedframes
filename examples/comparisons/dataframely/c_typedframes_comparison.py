"""Show typedframes catching column-level errors that dataframely misses.

typedframes uses a static analysis checker that validates Annotated[DataFrame, Schema]
against schema definitions, catching column name typos that mypy alone would miss.
"""

from typing import Annotated

import polars as pl

from typedframes import BaseSchema, Column


class OrderSchema(BaseSchema):
    """Schema for order data using typedframes."""

    order_id = Column(type=int)
    customer_name = Column(type=str)
    product_sku = Column(type=str)
    quantity = Column(type=int)
    unit_price = Column(type=float)
    total = Column(type=float)
    shipped = Column(type=bool)


def process_orders_correct() -> None:
    """Correct use of OrderSchema — checker is happy.

    Demonstrates column access that validates correctly against the schema.
    """
    df: Annotated[pl.DataFrame, OrderSchema] = pl.DataFrame(
        {
            "order_id": [1, 2, 3],
            "customer_name": ["Alice", "Bob", "Charlie"],
            "product_sku": ["SKU-001", "SKU-002", "SKU-003"],
            "quantity": [2, 1, 2],
            "unit_price": [29.99, 49.99, 19.99],
            "total": [59.98, 49.99, 39.98],
            "shipped": [True, True, False],
        },
    )

    # Correct column access — both mypy and typedframes are happy
    customer = df["customer_name"]
    print(f"Customers: {customer.to_list()}")


def process_orders_with_typos() -> None:
    """Example showing typos caught by typedframes checker.

    These column accesses have typos/errors that the typedframes checker
    catches at static analysis time, even though mypy sees Any and is silent.
    """
    df: Annotated[pl.DataFrame, OrderSchema] = pl.DataFrame(
        {
            "order_id": [1, 2, 3],
            "customer_name": ["Alice", "Bob", "Charlie"],
            "product_sku": ["SKU-001", "SKU-002", "SKU-003"],
            "quantity": [2, 1, 2],
            "unit_price": [29.99, 49.99, 19.99],
            "total": [59.98, 49.99, 39.98],
            "shipped": [True, True, False],
        },
    )

    # TYPO 1: 'custmer_name' instead of 'customer_name'
    # typedframes checker flags: unknown-column: Column 'custmer_name' not in OrderSchema
    _ = df["custmer_name"]

    # TYPO 2: 'unit_cost' instead of 'unit_price'
    # typedframes checker flags: unknown-column: Column 'unit_cost' not in OrderSchema
    _ = df["unit_cost"]


def process_orders_with_pl_col_typo() -> None:
    """Example with polars pl.col() — also caught by typedframes.

    typedframes checker validates pl.col() string references against schema.
    """
    df: Annotated[pl.DataFrame, OrderSchema] = pl.DataFrame(
        {
            "order_id": [1, 2, 3],
            "customer_name": ["Alice", "Bob", "Charlie"],
            "product_sku": ["SKU-001", "SKU-002", "SKU-003"],
            "quantity": [2, 1, 2],
            "unit_price": [29.99, 49.99, 19.99],
            "total": [59.98, 49.99, 39.98],
            "shipped": [True, True, False],
        },
    )

    # TYPO: 'unit_cost' in pl.col() — typedframes checker catches this
    _ = df.filter(pl.col("unit_cost") > 20)

    # CORRECT: Use existing column
    filtered = df.filter(pl.col("unit_price") > 20)
    print(f"High-price orders: {filtered.height}")


if __name__ == "__main__":
    print("Processing orders (correct schema)...")
    process_orders_correct()
    print("\nDone!")

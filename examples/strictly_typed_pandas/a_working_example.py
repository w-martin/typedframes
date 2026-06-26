"""
Working example of strictly-typed-pandas with static type checking.

Demonstrates idiomatic strictly-typed-pandas usage with DataSet[Schema] type hints.
This example should run without errors and pass mypy checks.
"""

from strictly_typed_pandas import DataSet


class OrderSchema:
    """Schema definition for orders dataset."""

    order_id: int
    customer_name: str
    product_sku: str
    quantity: int
    unit_price: float
    total: float
    shipped: bool


def load_orders() -> DataSet[OrderSchema]:
    """Load orders data with schema enforcement."""
    return DataSet[OrderSchema](
        {
            "order_id": [1, 2, 3],
            "customer_name": ["Alice", "Bob", "Charlie"],
            "product_sku": ["SKU-001", "SKU-002", "SKU-003"],
            "quantity": [2, 1, 5],
            "unit_price": [9.99, 24.99, 14.99],
            "total": [19.98, 24.99, 74.95],
            "shipped": [True, False, True],
        }
    )


def process_orders(orders: DataSet[OrderSchema]) -> None:
    """Process orders and print information."""
    print("Orders loaded:")
    print(f"  Total orders: {len(orders)}")
    print(f"  Columns: {list(orders.columns)}")

    # Access columns via dictionary-style access with type inference
    print(f"\nFirst order_id: {orders['order_id'].iloc[0]}")
    print(f"First customer_name: {orders['customer_name'].iloc[0]}")

    # Filter orders
    shipped_orders = orders[orders["shipped"]]
    print(f"\nShipped orders: {len(shipped_orders)}")

    # Calculate statistics
    total_revenue = orders["total"].sum()
    avg_quantity = orders["quantity"].mean()
    print(f"Total revenue: ${total_revenue:.2f}")
    print(f"Average quantity: {avg_quantity:.1f}")


def main() -> None:
    """Main entry point."""
    orders = load_orders()
    process_orders(orders)


if __name__ == "__main__":
    main()

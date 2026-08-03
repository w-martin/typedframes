"""StaticFrame working example with typed frames."""

import static_frame as sf


def create_orders_frame() -> sf.Frame:
    """Create a sample orders frame with typed columns.

    Returns:
        A StaticFrame with order data.
    """
    data = {
        "order_id": [1, 2, 3, 4, 5],
        "customer_name": ["Alice", "Bob", "Charlie", "Diana", "Eve"],
        "product_sku": ["SKU001", "SKU002", "SKU003", "SKU001", "SKU004"],
        "quantity": [5, 3, 10, 2, 1],
        "unit_price": [10.5, 20.0, 15.75, 10.5, 99.99],
        "total": [52.5, 60.0, 157.5, 21.0, 99.99],
        "shipped": [True, False, True, True, False],
    }

    return sf.Frame.from_dict(data)


def filter_shipped_orders(orders: sf.Frame) -> sf.Frame:
    """Filter to only shipped orders.

    Args:
        orders: Frame with order data.

    Returns:
        Frame containing only shipped orders.
    """
    # StaticFrame supports boolean indexing
    return orders.loc[orders["shipped"]]


def calculate_order_summary(orders: sf.Frame) -> None:
    """Calculate and print summary statistics.

    Args:
        orders: Frame with order data.
    """
    print("=== Orders Frame ===")
    print(orders)
    print(f"\nShape: {orders.shape}")
    print(f"Columns: {orders.columns.values.tolist()}")
    print(f"\nDtypes:\n{orders.dtypes}")

    # Calculate totals
    total_revenue = orders["total"].sum()
    shipped_count = orders["shipped"].sum()

    print(f"\nTotal Revenue: ${total_revenue:.2f}")
    print(f"Orders Shipped: {shipped_count}/{len(orders)}")


def transform_prices(orders: sf.Frame) -> sf.Frame:
    """Apply a price increase to all items.

    Args:
        orders: Frame with order data.

    Returns:
        Frame with updated prices (10% increase).
    """
    # StaticFrame operations return new frames (immutable)

    # Create a new frame with updated total prices
    new_totals = orders["total"] * 1.1

    # StaticFrame doesn't allow direct column assignment like pandas
    # We need to create a new frame with the modified data
    data = {}
    for col in orders.columns:
        if col == "total":
            data[col] = new_totals.values
        else:
            data[col] = orders[col].values

    return sf.Frame.from_dict(data)


if __name__ == "__main__":
    # Create orders frame
    orders = create_orders_frame()

    # Show summary
    calculate_order_summary(orders)

    # Filter shipped orders
    print("\n=== Shipped Orders Only ===")
    shipped = filter_shipped_orders(orders)
    print(shipped)

    # Transform prices
    print("\n=== With 10% Price Increase ===")
    increased = transform_prices(orders)
    print(increased)

"""Working example of dataframely usage with runtime validation."""

import dataframely as dy
import polars as pl


class OrderSchema(dy.Schema):
    """Schema for order data."""

    order_id = dy.Int64()
    customer_name = dy.String()
    product_sku = dy.String()
    quantity = dy.Int64()
    unit_price = dy.Float64()
    total = dy.Float64()
    shipped = dy.Bool()


def calculate_revenue(df: dy.DataFrame[OrderSchema]) -> float:
    """Calculate total revenue from orders.

    Args:
        df: DataFrame with OrderSchema validation.

    Returns:
        Total revenue as sum of 'total' column.
    """
    return df["total"].sum()


def filter_shipped_orders(df: dy.DataFrame[OrderSchema]) -> dy.DataFrame[OrderSchema]:
    """Filter to only shipped orders.

    Args:
        df: DataFrame with OrderSchema validation.

    Returns:
        Filtered DataFrame containing only shipped orders.
    """
    return df.filter(df["shipped"])


def get_customer_names(df: dy.DataFrame[OrderSchema]) -> dy.Series[dy.String]:
    """Extract customer names.

    Args:
        df: DataFrame with OrderSchema validation.

    Returns:
        Series of customer names.
    """
    return df["customer_name"]


if __name__ == "__main__":
    # Create sample data as Polars DataFrame
    data = {
        "order_id": [1, 2, 3, 4, 5],
        "customer_name": ["Alice", "Bob", "Charlie", "Diana", "Eve"],
        "product_sku": ["SKU-001", "SKU-002", "SKU-001", "SKU-003", "SKU-002"],
        "quantity": [2, 1, 3, 2, 1],
        "unit_price": [29.99, 49.99, 29.99, 19.99, 49.99],
        "total": [59.98, 49.99, 89.97, 39.98, 49.99],
        "shipped": [True, True, False, True, False],
    }

    df_polars = pl.DataFrame(data)

    # Cast to typed dataframe with schema validation
    df_typed = OrderSchema.validate(df_polars)

    # Use typed functions
    revenue = calculate_revenue(df_typed)
    print(f"Total revenue: ${revenue:.2f}")

    shipped_df = filter_shipped_orders(df_typed)
    print(f"Shipped orders count: {shipped_df.height}")

    customers = get_customer_names(df_typed)
    print(f"Customer names: {customers.to_list()}")

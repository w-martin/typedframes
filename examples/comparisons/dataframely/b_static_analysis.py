"""Demonstrate what mypy catches and misses with dataframely."""

import dataframely as dy
import polars as pl


class OrderSchema(dy.Schema):
    """Schema for order data."""

    order_id = dy.Int64()
    customer_name = dy.String()
    unit_price = dy.Float64()
    total = dy.Float64()
    shipped = dy.Bool()
    product_sku = dy.String()
    quantity = dy.Int64()


class OtherSchema(dy.Schema):
    """Different schema with only order_id."""

    order_id = dy.Int64()


def process(df: dy.DataFrame[OrderSchema]) -> None:
    """Process a DataFrame with OrderSchema.

    Args:
        df: DataFrame typed with OrderSchema.
    """
    # Typo in column name — mypy does NOT catch this (returns Any)
    try:
        x = df["custmer_name"]
        print(f"Value (typo, but silent): {x}")
    except Exception as e:
        print(f"Typo 'custmer_name' caught at runtime: {type(e).__name__}")

    # Wrong column name — mypy does NOT catch this (returns Any)
    try:
        y = df["unit_cost"]
        print(f"Value (wrong name, but silent): {y}")
    except Exception as e:
        print(f"Wrong name 'unit_cost' caught at runtime: {type(e).__name__}")

    # Correct column name — mypy does NOT check this
    z = df["customer_name"]
    print(f"Value (correct, but not checked by mypy): {z.to_list()}")


def wrong_schema(df: dy.DataFrame[OtherSchema]) -> None:
    """Accept different schema.

    Args:
        df: DataFrame typed with OtherSchema.
    """
    # mypy WILL catch this — generic parameter mismatch
    process(df)  # type: ignore[arg-type]


if __name__ == "__main__":
    # Create sample data
    data = {
        "order_id": [1, 2, 3],
        "customer_name": ["Alice", "Bob", "Charlie"],
        "unit_price": [29.99, 49.99, 19.99],
        "total": [59.98, 49.99, 39.98],
        "shipped": [True, True, False],
        "product_sku": ["SKU-001", "SKU-002", "SKU-003"],
        "quantity": [2, 1, 2],
    }

    df_polars = pl.DataFrame(data)
    df_typed = OrderSchema.validate(df_polars)

    print("Testing with correct schema:")
    process(df_typed)

    # Test with wrong schema
    other_data = {"order_id": [1, 2, 3]}
    other_df = pl.DataFrame(other_data)
    other_typed = OtherSchema.validate(other_df)

    print("\nTesting schema type mismatch (caught by mypy):")
    try:
        wrong_schema(other_typed)
    except Exception as e:
        print(f"Runtime error (expected at type check time): {e}")

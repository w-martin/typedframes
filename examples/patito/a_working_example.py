"""
Demonstrating idiomatic patito usage with runtime validation.

patito provides runtime validation for DataFrames against a defined Model.
This example shows both successful and failed validations.
"""

import pandas as pd
import patito as pt
import polars as pl


class OrderModel(pt.Model):
    """Order schema with type constraints."""

    order_id: int
    customer_name: str
    product_sku: str
    quantity: int
    unit_price: float
    total: float
    shipped: bool


def validate_polars_success() -> None:
    """Create a valid Polars DataFrame and validate it."""
    print("=" * 60)
    print("Test 1: Valid Polars DataFrame")
    print("=" * 60)

    df = pl.DataFrame(
        {
            "order_id": [1, 2, 3],
            "customer_name": ["Alice", "Bob", "Charlie"],
            "product_sku": ["SKU001", "SKU002", "SKU003"],
            "quantity": [5, 10, 2],
            "unit_price": [29.99, 49.99, 99.99],
            "total": [149.95, 499.90, 199.98],
            "shipped": [True, False, True],
        }
    )

    try:
        OrderModel.validate(df)
        print("✓ Validation passed")
        print(f"  Shape: {validated.shape}")
        print(f"  Columns: {validated.columns}")
    except Exception as e:
        print(f"✗ Validation failed: {e}")


def validate_polars_missing_column() -> None:
    """Try to validate DataFrame with missing column."""
    print("\n" + "=" * 60)
    print("Test 2: Polars DataFrame with missing column")
    print("=" * 60)

    df = pl.DataFrame(
        {
            "order_id": [1, 2],
            "customer_name": ["Alice", "Bob"],
            "product_sku": ["SKU001", "SKU002"],
            "quantity": [5, 10],
            # Missing unit_price, total, shipped
            "unit_price": [29.99, 49.99],
            "total": [149.95, 499.90],
        }
    )

    try:
        OrderModel.validate(df)
        print("✓ Validation passed")
    except Exception as e:
        print("✗ Validation failed (expected)")
        print(f"  Error type: {type(e).__name__}")
        print(f"  Message: {e}")


def validate_polars_wrong_type() -> None:
    """Try to validate DataFrame with wrong column type."""
    print("\n" + "=" * 60)
    print("Test 3: Polars DataFrame with wrong type (quantity as float)")
    print("=" * 60)

    df = pl.DataFrame(
        {
            "order_id": [1, 2],
            "customer_name": ["Alice", "Bob"],
            "product_sku": ["SKU001", "SKU002"],
            "quantity": [5.5, 10.5],  # Should be int, not float
            "unit_price": [29.99, 49.99],
            "total": [149.95, 499.90],
            "shipped": [True, False],
        }
    )

    try:
        OrderModel.validate(df)
        print("✓ Validation passed")
    except Exception as e:
        print("✗ Validation failed (expected)")
        print(f"  Error type: {type(e).__name__}")
        print(f"  Message: {e}")


def validate_pandas_success() -> None:
    """Create a valid Pandas DataFrame and validate it (requires pyarrow)."""
    print("\n" + "=" * 60)
    print("Test 4: Valid Pandas DataFrame (with pyarrow)")
    print("=" * 60)

    df = pd.DataFrame(
        {
            "order_id": [1, 2, 3],
            "customer_name": ["Alice", "Bob", "Charlie"],
            "product_sku": ["SKU001", "SKU002", "SKU003"],
            "quantity": [5, 10, 2],
            "unit_price": [29.99, 49.99, 99.99],
            "total": [149.95, 499.90, 199.98],
            "shipped": [True, False, True],
        }
    )

    try:
        # patito converts pandas to polars internally
        OrderModel.validate(df)
        print("✓ Validation passed")
        print(f"  Type of result: {type(validated).__name__}")
        print(f"  Shape: {validated.shape}")
    except Exception as e:
        print(f"✗ Validation failed: {e}")


def validate_pandas_wrong_type() -> None:
    """Try to validate Pandas DataFrame with wrong column type."""
    print("\n" + "=" * 60)
    print("Test 5: Pandas DataFrame with wrong type (shipped as int)")
    print("=" * 60)

    df = pd.DataFrame(
        {
            "order_id": [1, 2],
            "customer_name": ["Alice", "Bob"],
            "product_sku": ["SKU001", "SKU002"],
            "quantity": [5, 10],
            "unit_price": [29.99, 49.99],
            "total": [149.95, 499.90],
            "shipped": [1, 0],  # Should be bool
        }
    )

    try:
        OrderModel.validate(df)
        print("✓ Validation passed")
    except Exception as e:
        print("✗ Validation failed (expected)")
        print(f"  Error type: {type(e).__name__}")
        print(f"  Message: {e}")


if __name__ == "__main__":
    print("\nPatito Runtime Validation Examples")
    print("=" * 60)
    print("patito version: 0.8.6+")
    print("Runtime validation catches schema mismatches at runtime.\n")

    validate_polars_success()
    validate_polars_missing_column()
    validate_polars_wrong_type()
    validate_pandas_success()
    validate_pandas_wrong_type()

    print("\n" + "=" * 60)
    print("Summary: Patito provides runtime validation only.")
    print("No static analysis or type checking support.")
    print("=" * 60)

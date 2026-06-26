"""
Great Expectations core concept: runtime data validation.

GX v1.18 simplified demo showing:
- Creating expectations against a DataFrame
- Defining validation rules
- What GX validates vs. what it doesn't

Note: GX v1.18+ provides more complex APIs for pipeline integration.
This example focuses on the core validation concept.
"""

import pandas as pd


def main() -> None:
    # Create test data
    df = pd.DataFrame(
        {
            "order_id": [1, 2, 3],
            "customer_name": ["Alice", "Bob", "Charlie"],
            "product_sku": ["SKU-1", "SKU-2", "SKU-3"],
            "quantity": [2, 1, 5],
            "unit_price": [9.99, 24.99, 15.50],
            "total": [19.98, 24.99, 77.50],
            "shipped": [True, False, True],
        }
    )

    print("=" * 60)
    print("Great Expectations: Runtime Data Validation")
    print("=" * 60)
    print(f"DataFrame shape: {df.shape}")
    print(f"Columns: {list(df.columns)}")
    print(f"Dtypes:\n{df.dtypes}")

    # GX validates data quality at runtime
    print("\n" + "=" * 60)
    print("What GX Validates (Data Quality):")
    print("=" * 60)
    print("✓ Column existence: Does 'customer_name' exist?")
    print("✓ Column types: Is 'order_id' numeric?")
    print("✓ Nullability: Are there null values in 'customer_name'?")
    print("✓ Value ranges: Is 'quantity' > 0?")
    print("✓ Distribution: Is data anomalous?")
    print("✓ Schema changes: Did schema change since last run?")

    print("\n" + "=" * 60)
    print("What GX Does NOT Validate (Code-Level):")
    print("=" * 60)
    print("✗ df['custmer_name'] typo → not caught by GX")
    print("✗ df['unit_cost'] wrong name → not caught by GX")
    print("✗ Static type checking → not provided by GX")
    print("✗ Preventing KeyError → not caught before runtime")

    print("\n" + "=" * 60)
    print("Typical GX Validation Code:")
    print("=" * 60)
    print("""
# Create context and datasource
context = gx.get_context()
datasource = context.sources.add_pandas('data')

# Add expectations
validator.expect_column_to_exist('order_id')
validator.expect_column_values_to_not_be_null('customer_name')
validator.expect_column_values_to_be_in_set('shipped', [True, False])

# Run validation
checkpoint = context.add_or_update_checkpoint(...)
result = checkpoint.run()
print(f"Validation passed: {result.success}")
    """)

    # Show the actual data
    print("\n" + "=" * 60)
    print("Sample Data:")
    print("=" * 60)
    print(df.head())


if __name__ == "__main__":
    main()

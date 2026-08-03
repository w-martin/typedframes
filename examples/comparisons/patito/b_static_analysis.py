"""
Demonstrating patito's lack of static type analysis.

patito has no py.typed marker and does not provide type stubs.
mypy cannot analyze the resulting DataFrame type, so typos and
incorrect column accesses are not caught at lint-time.
"""

import patito as pt
import polars as pl


class OrderModel(pt.Model):
    """Order schema."""

    order_id: int
    customer_name: str
    product_sku: str
    quantity: int
    unit_price: float
    total: float
    shipped: bool


# Create and validate a DataFrame
df = pl.DataFrame(
    {
        "order_id": [1, 2],
        "customer_name": ["Alice", "Bob"],
        "product_sku": ["SKU001", "SKU002"],
        "quantity": [5, 10],
        "unit_price": [29.99, 49.99],
        "total": [149.95, 499.90],
        "shipped": [True, False],
    }
)

validated = OrderModel.validate(df)

# Typo in column name — mypy will NOT catch this
# It should be "customer_name" but we wrote "custmer_name"
typo_result = validated["custmer_name"]

# Non-existent column — mypy will NOT catch this
# This column does not exist in the schema
missing_col = validated["unit_cost"]

# Accessing with a literal string that has a typo
# mypy has no type information for validated, so it can't help
wrong_access = validated["ordr_id"]

print("If mypy was run on this file, it would say:")
print("  error: Skipping analyzing 'patito': module installed but")
print("    missing library stubs or py.typed marker (mypy-d1020)")
print("\nNo column access errors would be detected.")

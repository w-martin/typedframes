"""Demonstration of what typedframes checker catches."""

from typing import Annotated

import pandas as pd

from typedframes import BaseSchema, Column


class OrderSchema(BaseSchema):
    """Schema for orders."""

    order_id = Column(type=int)
    customer_name = Column(type=str)
    total = Column(type=float)


def test_checker_detection() -> None:
    """Test that the checker detects column typos."""
    # Create a DataFrame with the schema
    df = pd.DataFrame(
        {
            "order_id": [1, 2],
            "customer_name": ["Alice", "Bob"],
            "total": [52.5, 60.0],
        }
    )

    # Annotate with schema - triggers checker
    df_annot: Annotated[pd.DataFrame, OrderSchema] = df

    # Correct access - no error
    result1 = df_annot["order_id"]
    print(f"Order IDs: {result1}")

    # Typo - should be caught
    result2 = df_annot["custmer_name"]  # ERROR: did you mean 'customer_name'?
    print(f"Customers: {result2}")

    # Nonexistent column - should be caught
    result3 = df_annot["revenue"]  # ERROR: Column 'revenue' not in OrderSchema
    print(f"Revenue: {result3}")

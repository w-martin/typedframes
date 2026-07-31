"""Comparison: StaticFrame vs typedframes approach to type safety."""

from typing import Annotated

import pandas as pd
import static_frame as sf

from typedframes import BaseSchema, Column

# ============================================================================
# APPROACH 1: typedframes with pandas (zero rewrite required)
# ============================================================================


class OrderSchema(BaseSchema):
    """Schema for orders DataFrame."""

    order_id = Column(type=int)
    customer_name = Column(type=str)
    product_sku = Column(type=str)
    quantity = Column(type=int)
    unit_price = Column(type=float)
    total = Column(type=float)
    shipped = Column(type=bool)


def process_orders_typedframes(
    df: Annotated[pd.DataFrame, OrderSchema],
) -> pd.Series:
    """Process orders using typedframes annotation.

    With the typedframes checker, accessing a nonexistent column
    is caught at static analysis time. No API changes to pandas code.

    Args:
        df: A pandas DataFrame annotated with OrderSchema.

    Returns:
        The customer_name Series.
    """
    # Annotate locally to trigger checking
    df_annot: Annotated[pd.DataFrame, OrderSchema] = df
    # This is caught by typedframes checker:
    return df_annot["custmer_name"]  # ERROR: did you mean 'customer_name'?


def get_shipped_orders(
    df: Annotated[pd.DataFrame, OrderSchema],
) -> pd.DataFrame:
    """Filter to shipped orders.

    Args:
        df: A pandas DataFrame annotated with OrderSchema.

    Returns:
        DataFrame containing only shipped orders.
    """
    return df[df["shipped"]]


def calculate_revenue(
    df: Annotated[pd.DataFrame, OrderSchema],
) -> float:
    """Calculate total revenue.

    Args:
        df: A pandas DataFrame annotated with OrderSchema.

    Returns:
        Total revenue as float.
    """
    return float(df["total"].sum())


# ============================================================================
# APPROACH 2: StaticFrame (requires full rewrite, but type-safe by design)
# ============================================================================


def process_orders_staticframe(frame: sf.Frame) -> sf.Series:
    """Process orders using StaticFrame.

    StaticFrame is a completely different DataFrame library.
    Column access returns Series objects from StaticFrame, not pandas.

    Args:
        frame: A StaticFrame.

    Returns:
        The customer_name Series (StaticFrame Series, not pandas Series).
    """
    # StaticFrame doesn't provide static typing for column names at type-check time
    # You would need to use different column access patterns for runtime safety
    return frame["custmer_name"]


# ============================================================================
# KEY DIFFERENCES
# ============================================================================

# TYPEDFRAMES:
# - Works with existing pandas/polars code
# - Zero API changes to your DataFrame operations
# - Column typos caught by mypy plugin
# - Can migrate gradually (annotate one function at a time)
# - Example: def func(df: Annotated[pd.DataFrame, Schema]) -> ...

# STATICFRAME:
# - Immutable by design
# - Different API (not compatible with pandas code)
# - Must rewrite all DataFrame code
# - Column access is runtime string-based (not statically typed)
# - Example: frame = sf.Frame.from_dict(data)

# ============================================================================
# RUNTIME EXAMPLES
# ============================================================================


def demonstrate_typedframes() -> None:
    """Show typedframes with actual pandas code."""
    import pandas as pd

    # Existing pandas code, zero changes needed
    df = pd.DataFrame(
        {
            "order_id": [1, 2, 3],
            "customer_name": ["Alice", "Bob", "Charlie"],
            "product_sku": ["SKU001", "SKU002", "SKU003"],
            "quantity": [5, 3, 10],
            "unit_price": [10.5, 20.0, 15.75],
            "total": [52.5, 60.0, 157.5],
            "shipped": [True, False, True],
        }
    )

    # All pandas methods work exactly as before
    shipped = get_shipped_orders(df)
    revenue = calculate_revenue(df)

    print(f"Shipped orders:\n{shipped}")
    print(f"Total revenue: ${revenue:.2f}")


def demonstrate_staticframe() -> None:
    """Show StaticFrame (different library, different API)."""
    import static_frame as sf

    # StaticFrame creation (different API from pandas)
    frame = sf.Frame.from_dict(
        {
            "order_id": [1, 2, 3],
            "customer_name": ["Alice", "Bob", "Charlie"],
            "product_sku": ["SKU001", "SKU002", "SKU003"],
            "quantity": [5, 3, 10],
            "unit_price": [10.5, 20.0, 15.75],
            "total": [52.5, 60.0, 157.5],
            "shipped": [True, False, True],
        }
    )

    # StaticFrame operations (immutable, different API)
    shipped = frame.loc[frame["shipped"]]
    revenue = frame["total"].sum()

    print(f"Shipped orders:\n{shipped}")
    print(f"Total revenue: {revenue}")


if __name__ == "__main__":
    print("=== typedframes (pandas-based) ===")
    demonstrate_typedframes()

    print("\n=== StaticFrame ===")
    demonstrate_staticframe()

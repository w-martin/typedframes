"""Example demonstrating pandera schema validation vs typedframes approach."""

import pandera as pa  # ty: ignore[unresolved-import]
from pandera.typing import DataFrame, Series  # ty: ignore[unresolved-import]


class UserSchema(pa.DataFrameModel):
    """Schema defining user data structure."""

    user_id: Series[int]
    email: Series[str]


class OrderSchema(pa.DataFrameModel):
    """Schema defining order data structure."""

    order_id: Series[int]
    amount: Series[float]


def load_users() -> DataFrame[UserSchema]:
    """Load sample user data."""
    return DataFrame[UserSchema](
        {"user_id": [1, 2], "email": ["foo@baz.com", "bar@qux.com"]},
    )


def process_orders(df: DataFrame[OrderSchema]) -> None:
    """Process orders - expects OrderSchema."""


def main() -> None:
    """Demonstrate schema validation use cases."""
    # Passing wrong schema to function
    # We load users (UserSchema) but pass to function expecting orders (OrderSchema)
    users = load_users()
    process_orders(users)
    # mypy catches this as a DataFrame type mismatch. typedframes' standalone
    # checker independently catches it too, via missing-column: process_orders'
    # OrderSchema-annotated parameter requires {order_id, amount}, but 'users'
    # only carries {user_id, email} — no type checker needed for this one.

    # Issue 1: Accessing non-existent column
    users = load_users()
    # 'name' column doesn't exist in UserSchema — pandera/mypy miss this
    print(users["name"])  # typedframes: unknown-column Column 'name' does not exist in UserSchema

    # Issue 2: Mutation breaks tracking
    users = load_users()  # Has UserSchema
    users["new_column"] = 123  # Now has extra column
    # DESIRED: Warning/Error about untracked schema changes

    # Issue 3: Column name typo
    users = load_users()
    # pandera/mypy miss this typo — typedframes catches it
    print(users["emai"])  # typedframes: unknown-column Column 'emai' does not exist in UserSchema

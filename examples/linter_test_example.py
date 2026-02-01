"""Example file used by integration tests to verify linter functionality."""

from typedframes import BaseSchema, Column, ColumnSet


class UserSchema(BaseSchema):
    """Schema for user data."""

    user_id = Column(type=int)
    email = Column(type=str)


class OrderSchema(BaseSchema):
    """Schema for order data."""

    order_id = Column(type=int)
    amount = Column(type=float)


def main() -> None:
    """Test various linting scenarios."""
    # Valid column access
    users: DataFrame[UserSchema] = load_users()
    print(users["user_id"])
    print(users["email"])

    # Error: Column 'name' does not exist in UserSchema
    print(users["name"])

    # Error: Column 'emai' does not exist (typo)
    print(users["emai"])

    # Mutation tracking
    users["new_column"] = 123

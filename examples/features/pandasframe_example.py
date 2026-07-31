"""Pandas example — Annotated type annotation with column validation."""

from typing import Annotated

import pandas as pd

from typedframes import BaseSchema, Column


class UserSchema(BaseSchema):
    """Schema for user data."""

    user_id = Column(type=int)
    email = Column(type=str)


def load_users() -> Annotated[pd.DataFrame, UserSchema]:
    """Load user records and assert UserSchema."""
    return pd.read_csv("users.csv", usecols=["user_id", "email"])


def main() -> None:
    """Demonstrate Annotated[pd.DataFrame, Schema] with column validation."""
    df: Annotated[pd.DataFrame, UserSchema] = pd.DataFrame({"user_id": [1], "email": ["a@b.com"]})

    # String access — validated by the checker
    print(df["email"])
    print(df["user_id"])

    # These would be caught by the checker:
    print(df["name"])  # ✗ unknown-column: Column 'name' not in UserSchema
    print(df["emai"])  # ✗ unknown-column: Column 'emai' not in UserSchema (did you mean 'email'?)

    # .s gives a refactor-safe string name from the descriptor
    print(df[UserSchema.email.s])  # same as df["email"]
    print(df[UserSchema.user_id.s])  # same as df["user_id"]

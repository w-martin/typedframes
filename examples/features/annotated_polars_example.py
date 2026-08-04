"""Polars example: Annotated[pl.DataFrame, Schema] with pl.col() validation.

Annotate a plain polars DataFrame with a schema, then use ordinary `pl.col()`
expressions and string/subscript access exactly as you already do -- the checker
validates both against the schema at lint time, with zero runtime overhead. `.col`
gives a refactor-safe expression from the schema descriptor instead of a bare string.

(`PolarsFrame[Schema]` is a deprecated alternate spelling for this same annotation --
see `docs/api/polars.md` -- so this file sticks to the `Annotated[...]` form directly.)
"""

from typing import Annotated

import polars as pl

from typedframes import BaseSchema, Column


class UserSchema(BaseSchema):
    """Schema for user data."""

    user_id = Column(type=int)
    email = Column(type=str)


def load_users() -> Annotated[pl.DataFrame, UserSchema]:
    """Load user records and assert UserSchema."""
    return pl.read_csv("users.csv", columns=["user_id", "email"])


def main() -> None:
    """Demonstrate Annotated[pl.DataFrame, Schema] with pl.col() validation."""
    df: Annotated[pl.DataFrame, UserSchema] = pl.DataFrame({"user_id": [1], "email": ["a@b.com"]})

    # pl.col() references are validated by the checker
    print(df.select(pl.col("email")))
    print(df.select(pl.col("user_id")))

    # These would be caught by the checker:
    print(df["name"])  # ✗ unknown-column: Column 'name' not in UserSchema
    print(df["emai"])  # ✗ unknown-column: Column 'emai' not in UserSchema (did you mean 'email'?)
    print(df.filter(pl.col("emai").is_not_null()))  # ✗ unknown-column: 'emai' not in UserSchema

    # .col gives a refactor-safe polars expression from the descriptor
    print(df.select(UserSchema.email.col))  # same as df.select(pl.col("email"))
    print(df.filter(UserSchema.user_id.col > 0))

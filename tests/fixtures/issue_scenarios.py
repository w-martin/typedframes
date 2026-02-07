"""Test fixture: Original issue.md scenarios."""

from typing import Annotated

import polars as pl

from typedframes import BaseSchema, Column


class UserSchema(BaseSchema):
    """Schema defining user data structure."""

    user_id = Column(type=int)
    email = Column(type=str)


df: Annotated[pl.DataFrame, UserSchema] = pl.DataFrame({
    "user_id": [1, 2],
    "email": ["foo@baz.com", "bar@qux.com"],
})

# Issue 1: Accessing non-existent column
print(df["name"])  # Should error: Column 'name' does not exist in UserSchema

# Issue 2: Column name typo
print(df["emai"])  # Should error: Column 'emai' does not exist (did you mean 'email'?)

# Issue 3: Mutation tracking
df["new_column"] = 123  # Should warn: mutation tracking

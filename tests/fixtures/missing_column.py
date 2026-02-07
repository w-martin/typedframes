"""Test fixture: Accessing a non-existent column."""

from typing import Annotated

import polars as pl

from typedframes import BaseSchema, Column


class UserSchema(BaseSchema):
    """Test schema for user data."""

    user_id = Column(type=int)
    email = Column(type=str)


df: Annotated[pl.DataFrame, UserSchema] = pl.DataFrame({
    "user_id": [1, 2],
    "email": ["a@b.com", "c@d.com"],
})

# This should be caught by the linter
print(df["non_existent"])

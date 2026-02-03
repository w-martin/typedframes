"""Example usage of typedframes for pandas and polars DataFrames."""

from typing import Annotated

import pandas as pd
import polars as pl

from typedframes import BaseSchema, Column, ColumnSet, PandasFrame


class UserSchema(BaseSchema):
    """Schema for user data."""

    user_id = Column(type=int)
    email = Column(type=str, alias="email_address")
    metadata = ColumnSet(members=["age", "gender"], type=str)


def pandas_example() -> None:
    """Demonstrate PandasFrame usage with schema-aware attribute access."""
    raw_df = pd.DataFrame(
        {
            "user_id": [1, 2, 3],
            "email_address": ["a@b.com", "c@d.com", "e@f.com"],
            "age": ["25", "30", "35"],
            "gender": ["M", "F", "M"],
        },
    )

    # Create a typed PandasFrame
    df: PandasFrame[UserSchema] = PandasFrame.from_schema(raw_df, UserSchema)

    # Access columns by schema attribute names
    print("User IDs:", df.user_id.tolist())
    print("Emails:", df["email_address"].tolist())  # Uses alias name

    # Schema operations preserve type
    filtered = df[df.user_id > 1]
    print("Filtered schema:", filtered.schema)

    # This would be caught by the linter:
    print(df["wrong_column"])  # Error: Column 'wrong_column' does not exist
    print(df["user_i"])  # Error: Column 'user_i' does not exist (typo)


def polars_example() -> None:
    """Demonstrate PolarsFrame usage with full autocomplete."""
    df: Annotated[pl.DataFrame, UserSchema] = pl.DataFrame(
        {
            "user_id": [1, 2, 3],
            "email_address": ["a@b.com", "c@d.com", "e@f.com"],
            "age": ["25", "30", "35"],
            "gender": ["M", "F", "M"],
        },
    )

    # Full polars API with autocomplete
    result = df.filter(pl.col("user_id") > 1)
    print("Filtered polars:\n", result)

    # Use schema column references for type-safe access
    result2 = df.filter(UserSchema.user_id.col > 1)
    print("Schema-based filter:\n", result2)

    # Select using schema columns
    result3 = df.select(UserSchema.user_id.col, UserSchema.email.col)
    print("Selected columns:\n", result3)

    # This would be caught by the linter:
    print(df["typo_column"])  # Error: Column 'typo_column' does not exist
    print(df["user_id_typo"])

    # Mutation tracking: adding a column that's not in the schema
    # (In standard pandas this is allowed, but typedframes linter can flag it)
    df["new_column"] = 123


if __name__ == "__main__":
    print("=== Pandas Example ===")
    pandas_example()

    print("\n=== Polars Example ===")
    polars_example()

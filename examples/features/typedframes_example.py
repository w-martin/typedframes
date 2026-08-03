"""Example usage of typedframes for pandas and polars DataFrames."""

from typing import Annotated

import pandas as pd
import polars as pl

from typedframes import BaseSchema, Column, ColumnSet


class UserSchema(BaseSchema):
    """Schema for user data."""

    user_id = Column(type=int)
    email = Column(type=str, alias="email_address")
    metadata = ColumnSet(members=["age", "gender"], type=str)


def pandas_example() -> None:
    """Demonstrate Annotated[pd.DataFrame, Schema] with column validation."""
    df: Annotated[pd.DataFrame, UserSchema] = pd.DataFrame(
        {
            "user_id": [1, 2, 3],
            "email_address": ["a@b.com", "c@d.com", "e@f.com"],
            "age": ["25", "30", "35"],
            "gender": ["M", "F", "M"],
        },
    )

    # String access — validated by the checker
    print("User IDs:", df["user_id"].tolist())
    print("Emails:", df["email_address"].tolist())

    # .s gives a refactor-safe string name from the descriptor
    print("User IDs via descriptor:", df[UserSchema.user_id.s].tolist())

    # Metadata columns via .s on the ColumnSet
    print("Metadata columns:", df[UserSchema.metadata.s].head())


def pandas_checker_errors() -> None:
    """Checker demo — these errors are caught statically; this function is not called at runtime."""
    df: Annotated[pd.DataFrame, UserSchema] = pd.DataFrame({})
    print(df["wrong_column"])  # ✗ unknown-column: Column 'wrong_column' not in UserSchema
    print(df["user_i"])  # ✗ unknown-column: Column 'user_i' not in UserSchema (did you mean 'user_id'?)


def polars_example() -> None:
    """Demonstrate Annotated[pl.DataFrame, Schema] with pl.col() validation."""
    df: Annotated[pl.DataFrame, UserSchema] = pl.DataFrame(
        {
            "user_id": [1, 2, 3],
            "email_address": ["a@b.com", "c@d.com", "e@f.com"],
            "age": ["25", "30", "35"],
            "gender": ["M", "F", "M"],
        },
    )

    # pl.col() references are validated by the checker
    result = df.filter(pl.col("user_id") > 1)
    print("Filtered polars:\n", result)

    # .col gives a refactor-safe polars expression from the descriptor
    result2 = df.filter(UserSchema.user_id.col > 1)
    print("Schema-based filter:\n", result2)


def polars_checker_errors() -> None:
    """Checker demo — these errors are caught statically; this function is not called at runtime."""
    df: Annotated[pl.DataFrame, UserSchema] = pl.DataFrame({})
    print(df["typo_column"])  # ✗ unknown-column: Column 'typo_column' not in UserSchema
    print(df["user_id_typo"])  # ✗ unknown-column: Column 'user_id_typo' not in UserSchema


if __name__ == "__main__":
    print("=== Pandas Example ===")
    pandas_example()

    print("\n=== Polars Example ===")
    polars_example()

"""Demonstrates aggressive column inference in typedframes.

The standalone checker infers column sets from data loading calls and method
chains — without requiring explicit schema annotations. This example shows:

  1. Full schema annotation  (best practice — no warnings)
  2. usecols / columns=      (inferred schema — no warnings)
  3. No columns specified    (untracked-dataframe warning — off by default, enable with --strict-ingest)
  4. Method chain inference  (select, drop, rename, assign, filter)

untracked-dataframe is suppressed by default (EDA-friendly mode). To enable it for production CI:

    typedframes check src/ --strict-ingest

Or suppress all warnings for a single run:

    typedframes check src/ --no-warnings
"""

from __future__ import annotations

from typing import Annotated

import pandas as pd
import polars as pl

from typedframes import BaseSchema, Column


class UserData(BaseSchema):
    """User records schema."""

    user_id = Column(type=int)
    email = Column(type=str)
    age = Column(type=int)


# ---------------------------------------------------------------------------
# 1. Full schema annotation (best practice)
# ---------------------------------------------------------------------------
# Annotating the variable with a schema gives the checker complete column
# information. No warnings are generated.


def load_annotated() -> None:
    """Load with explicit schema annotation — full static checking."""
    df: Annotated[pd.DataFrame, UserData] = pd.read_csv("users.csv")
    print(df["user_id"])
    print(df["email"])
    # Accessing df["username"] would error: Column 'username' not in UserData


# ---------------------------------------------------------------------------
# 2. Explicit usecols= — inferred schema, no warning
# ---------------------------------------------------------------------------
# Passing usecols= (pandas) or columns= / schema= (polars) lets the checker
# build an inferred column set. Column access is validated against it.


def load_with_usecols() -> None:
    """Load with usecols= — checker infers column set, no untracked-dataframe warning."""
    df = pd.read_csv("users.csv", usecols=["user_id", "email"])
    print(df["user_id"])
    print(df["email"])
    # Accessing df["age"] would error: 'age' not in inferred column set


def load_polars_with_columns() -> None:
    """Polars read_csv with columns= — checker infers column set."""
    df = pl.read_csv("users.csv", columns=["user_id", "email"])
    print(df["user_id"])
    # Accessing df["age"] would error: 'age' not in inferred column set


# ---------------------------------------------------------------------------
# 2b. Parquet files — same inference, same validation
# ---------------------------------------------------------------------------
# read_parquet (pandas and polars) is treated identically to read_csv.
# Pass columns= to give the checker a column set.


def load_parquet_pandas() -> None:
    """Pandas read_parquet with columns= — checker infers column set."""
    df = pd.read_parquet("users.parquet", columns=["user_id", "email"])
    print(df["user_id"])
    print(df["email"])
    # Accessing df["age"] would error: 'age' not in inferred column set


def load_parquet_polars() -> None:
    """Polars read_parquet with columns= — same inference as read_csv."""
    df = pl.read_parquet("users.parquet", columns=["user_id", "email"])
    print(df["user_id"])
    # Accessing df["age"] would error: 'age' not in inferred column set


# ---------------------------------------------------------------------------
# 3. No columns specified — untracked-dataframe warning (off by default)
# ---------------------------------------------------------------------------
# Without usecols/columns or a schema annotation, the checker assumes an
# Unknown state and stays quiet. Run with --strict-ingest to enable the warning.
#
# Checker output (with --strict-ingest):
#   inference_example.py:N:1: warning[untracked-dataframe] columns unknown at lint time; specify
#     `usecols`/`columns` or annotate: `df: Annotated[pd.DataFrame, MySchema] = pd.read_csv(...)`


def load_without_columns() -> None:
    """Load without column info — generates untracked-dataframe with --strict-ingest."""
    df = pd.read_csv("users.csv")
    df_pl = pl.read_csv("users.csv")
    print(df, df_pl)


# ---------------------------------------------------------------------------
# 4. Method chain inference
# ---------------------------------------------------------------------------
# The checker propagates inferred column sets through method chains.
# Row-preserving operations (filter, query, head, tail, sort_values, dropna,
# fillna, ffill, bfill, reset_index, ...) pass the column set through unchanged.


def method_chain_inference() -> None:
    """Demonstrate column set propagation through pandas method chains."""
    df: Annotated[pd.DataFrame, UserData] = pd.read_csv("users.csv")

    # Subscript slice — inferred column set {user_id, email}
    small = df[["user_id", "email"]]
    print(small["user_id"])
    # Accessing small["age"] would error: 'age' not in inferred column set

    # Row filters pass the column set through unchanged
    filtered = small[small["user_id"] > 0]
    print(filtered["user_id"])
    # Accessing filtered["age"] would error: 'age' not in inferred column set

    # rename() updates the column set — old name removed, new name added
    renamed = small.rename(columns={"email": "email_address"})
    print(renamed["email_address"])
    # Accessing renamed["email"] would error: 'email' was renamed out

    # drop() removes columns from the inferred set
    trimmed = df.drop(columns=["age"])
    print(trimmed["user_id"])
    # Accessing trimmed["age"] would error: 'age' was dropped

    # assign() adds new columns to the inferred set
    augmented = df.assign(created_at="2024-01-01")
    print(augmented["created_at"])
    print(augmented["user_id"])
    # Accessing augmented["phone"] would error: 'phone' not in inferred column set


def polars_select_inference() -> None:
    """Demonstrate select() inference for polars."""
    df: Annotated[pl.DataFrame, UserData] = pl.read_csv("users.csv")

    # select() with a literal list — inferred column set {user_id, email}
    small = df.select(["user_id", "email"])
    print(small["user_id"])
    # Accessing small["age"] would error: 'age' not in inferred column set


# ---------------------------------------------------------------------------
# 5. Remaining gaps — where dropped-unknown-column still appears
# ---------------------------------------------------------------------------
# Even with inferred schemas, some patterns generate warnings.


def inference_gaps() -> None:
    """Show dropped-unknown-column: dropping a column not present in the inferred set."""
    df: Annotated[pd.DataFrame, UserData] = pd.read_csv("users.csv")

    # Dropping a column that the checker knows doesn't exist — dropped-unknown-column warning:
    # dropped-unknown-column: Dropped column 'nonexistent' does not exist in UserData
    trimmed = df.drop(columns=["nonexistent"])
    print(trimmed)

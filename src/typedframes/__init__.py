r"""
typedframes - Type-safe DataFrame schemas for pandas and polars.

Provides schema definitions for DataFrames that enable:
- Static analysis of column access (via Rust linter)
- Documentation as code for DataFrame structures
- Optional runtime validation integration with third-party tools

Core Classes:
    BaseSchema: Define DataFrame column schemas.
    Column: Define a single column with type and optional alias.
    ColumnSet: Define a group of columns matching a pattern.
    ColumnGroup: Group multiple Columns/ColumnSets for convenient access.

Frame Classes:
    PandasFrame: pandas DataFrame subclass with schema-aware attribute access.
    PolarsFrame: Type annotation for polars DataFrames (uses Annotated pattern).

Usage:
    from typing import Annotated
    import pandas as pd
    import polars as pl
    from typedframes import BaseSchema, Column, ColumnSet, PandasFrame, PolarsFrame

    class UserSchema(BaseSchema):
        user_id = Column(type=int)
        email = Column(type=str, alias="user_email")
        scores = ColumnSet(members=r"score_\\d+", type=float, regex=True)

    # Pandas: Use PandasFrame for schema-aware attribute access
    df: PandasFrame[UserSchema] = PandasFrame.from_schema(
        pd.read_csv("users.csv"),
        UserSchema
    )
    df.user_id  # Schema column access

    # Polars: Use Annotated for full autocomplete (recommended)
    df: Annotated[pl.DataFrame, UserSchema] = pl.read_csv("users.csv")
    df.filter(UserSchema.user_id.col > 10)  # Schema-based expressions

    # Or use PolarsFrame type alias
    df: PolarsFrame[UserSchema] = pl.read_csv("users.csv")
"""

__version__ = "0.1.0"

from .base_schema import BaseSchema as BaseSchema
from .column import Column as Column
from .column_group import ColumnGroup as ColumnGroup
from .column_group_error import ColumnGroupError as ColumnGroupError
from .column_set import ColumnSet as ColumnSet
from .pandas_frame import PandasFrame as PandasFrame
from .polars_frame import PolarsFrame as PolarsFrame
from .schema_algebra import SchemaConflictError as SchemaConflictError
from .schema_algebra import combine_schemas as combine_schemas

__all__ = [
    "BaseSchema",
    "Column",
    "ColumnGroup",
    "ColumnGroupError",
    "ColumnSet",
    "PandasFrame",
    "PolarsFrame",
    "SchemaConflictError",
    "combine_schemas",
]

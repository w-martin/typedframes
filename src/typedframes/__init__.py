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

Usage:
    from typing import Annotated
    import pandas as pd
    import polars as pl
    from typedframes import BaseSchema, Column, ColumnSet

    class UserSchema(BaseSchema):
        user_id = Column(type=int)
        email = Column(type=str, alias="user_email")
        scores = ColumnSet(members=r"score_\\d+", type=float, regex=True)

    # Recommended: use Annotated for both pandas and polars
    df: Annotated[pd.DataFrame, UserSchema] = pd.read_csv("users.csv")
    df['user_id']  # ✓ Validated by checker
    df[UserSchema.user_id.s]  # ✓ Refactor-safe string access via descriptor

    df: Annotated[pl.DataFrame, UserSchema] = pl.read_csv("users.csv")
    df.filter(pl.col('user_id') > 10)  # ✓ pl.col() validated by checker
    df.filter(UserSchema.user_id.col > 10)  # ✓ Refactor-safe polars expression
"""

__version__ = "0.13.0"

from importlib import import_module
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from .base_schema import BaseSchema as BaseSchema
    from .column import Column as Column
    from .column_group import ColumnGroup as ColumnGroup
    from .column_group_error import ColumnGroupError as ColumnGroupError
    from .column_set import ColumnSet as ColumnSet
    from .missing_dependency_error import MissingDependencyError as MissingDependencyError
    from .schema_algebra import SchemaConflictError as SchemaConflictError
    from .schema_algebra import combine_schemas as combine_schemas

# Imported on first use: the command line never touches these, and loading them costs a
# measurable share of every `typedframes check`.
_SUBMODULE_OF = {
    "BaseSchema": "base_schema",
    "Column": "column",
    "ColumnGroup": "column_group",
    "ColumnGroupError": "column_group_error",
    "ColumnSet": "column_set",
    "MissingDependencyError": "missing_dependency_error",
    "SchemaConflictError": "schema_algebra",
    "combine_schemas": "schema_algebra",
}

__all__ = [
    "BaseSchema",
    "Column",
    "ColumnGroup",
    "ColumnGroupError",
    "ColumnSet",
    "MissingDependencyError",
    "SchemaConflictError",
    "combine_schemas",
]


def __getattr__(name: str) -> object:
    """Import a public name from its submodule the first time it is asked for."""
    submodule = _SUBMODULE_OF.get(name)
    if submodule is None:
        msg = f"module {__name__!r} has no attribute {name!r}"
        raise AttributeError(msg)
    value = getattr(import_module(f".{submodule}", __name__), name)
    globals()[name] = value
    return value

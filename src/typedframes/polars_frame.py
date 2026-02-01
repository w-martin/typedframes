"""
PolarsFrame - Type annotations for schema-aware polars DataFrames.

Since polars DataFrames are Rust objects that cannot be effectively subclassed
(methods return pl.DataFrame, not the subclass), we use type annotations
rather than wrapper classes for polars support.

This approach:
- Preserves full polars autocomplete and introspection
- Allows the static linter to validate column access
- Has zero runtime overhead

Usage:
    from typing import Annotated
    from typedframes import BaseSchema, Column

    class UserSchema(BaseSchema):
        user_id = Column(type=int)
        email = Column(type=str)

    # Option 1: Using Annotated directly (recommended)
    df: Annotated[pl.DataFrame, UserSchema] = pl.read_csv("users.csv")

    # Option 2: Using PolarsFrame type alias
    df: PolarsFrame[UserSchema] = pl.read_csv("users.csv")

    # Full polars autocomplete works
    df.filter(pl.col("user_id") > 10)

    # Use schema columns for type-safe access
    df.filter(UserSchema.user_id.col > 10)
    df.select(UserSchema.email.col, UserSchema.user_id.col)
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Annotated, TypeVar

from .base_schema import BaseSchema

if TYPE_CHECKING:
    import polars as pl

SchemaT = TypeVar("SchemaT", bound=BaseSchema)


class PolarsFrame:
    """
    Type marker for schema-annotated polars DataFrames.

    This is a type-only construct - at runtime, PolarsFrame[Schema] returns
    Annotated[pl.DataFrame, Schema], meaning the actual value is a plain
    pl.DataFrame with full polars functionality.

    The typedframes linter parses the Annotated metadata to validate
    column access statically.

    Example:
        df: PolarsFrame[UserSchema] = pl.read_csv("users.csv")

        # This is equivalent to:
        df: Annotated[pl.DataFrame, UserSchema] = pl.read_csv("users.csv")

        # Full polars autocomplete and all methods work
        result = df.filter(pl.col("user_id") > 10).select("email")

        # Schema-based column access (for building expressions)
        df.filter(UserSchema.user_id.col > 10)
    """

    def __class_getitem__(cls, schema: type[SchemaT]) -> type:
        """
        Create an Annotated type combining pl.DataFrame with schema metadata.

        Args:
            schema: A BaseSchema subclass defining the DataFrame structure.

        Returns:
            Annotated[pl.DataFrame, schema] for type checking.
        """
        import polars as pl

        return Annotated[pl.DataFrame, schema]


__all__ = ["PolarsFrame"]

"""
PolarsFrame - Type annotations for schema-aware polars DataFrames.

Deprecated: use ``Annotated[pl.DataFrame, Schema]`` directly instead (Option 1
below). ``PolarsFrame[Schema]`` (Option 2) is just an alias for that -- polars
DataFrames are Rust objects that can't be meaningfully subclassed, so there was
never a real runtime subclass behind it, unlike ``PandasFrame`` (itself also
deprecated, for the asymmetry this creates). Under strict type checking,
``PolarsFrame[Schema]`` is declared as a nominal ``pl.DataFrame`` subclass so it gets
full autocomplete -- but that means assigning the plain ``pl.DataFrame`` it actually
always is at runtime looks like a Liskov substitution violation to a type checker.
``Annotated[...]`` alone is sufficient for everything the static checker validates,
without either maintenance-heavy runtime wrapper mechanism.

This approach:
- Preserves full polars autocomplete and introspection
- Allows the static checker to validate column access
- Has zero runtime overhead

Usage:
    from typing import Annotated
    from typedframes import BaseSchema, Column

    class UserSchema(BaseSchema):
        user_id = Column(type=int)
        email = Column(type=str)

    # Option 1: Using Annotated directly (recommended)
    df: Annotated[pl.DataFrame, UserSchema] = pl.read_csv("users.csv")

    # Option 2: Using PolarsFrame type alias (deprecated)
    df: PolarsFrame[UserSchema] = pl.read_csv("users.csv")

    # Full polars autocomplete works
    df.filter(pl.col("user_id") > 10)

    # Use schema columns for type-safe access
    df.filter(UserSchema.user_id.col > 10)
    df.select(UserSchema.email.col, UserSchema.user_id.col)
"""

from __future__ import annotations

import warnings
from typing import TYPE_CHECKING, Annotated, Any, Generic, TypeVar

from .base_schema import BaseSchema

SchemaT = TypeVar("SchemaT", bound=BaseSchema)

_DEPRECATION_MESSAGE = (
    "PolarsFrame is deprecated and will be removed in a future release. "
    "Use `Annotated[pl.DataFrame, Schema]` directly instead -- PolarsFrame[Schema] "
    "has always just been an alias for that, with no real runtime subclass behind it."
)

if TYPE_CHECKING:
    import polars as pl

    class PolarsFrame(pl.DataFrame, Generic[SchemaT]):  # type: ignore[misc]
        """
        Type stub for schema-annotated polars DataFrames.

        During type checking, PolarsFrame[Schema] is seen as a pl.DataFrame
        subclass, preserving full autocomplete and method resolution.

        At runtime, PolarsFrame[Schema] returns Annotated[pl.DataFrame, Schema]
        via ``__class_getitem__``, so the actual value is a plain pl.DataFrame.

        """

        @classmethod
        def read_csv(cls, source: Any, schema: type[SchemaT], **kwargs: Any) -> PolarsFrame[SchemaT]:
            """Type stub matching the runtime read_csv classmethod below."""
            ...

        @classmethod
        def read_parquet(cls, source: Any, schema: type[SchemaT], **kwargs: Any) -> PolarsFrame[SchemaT]:
            """Type stub matching the runtime read_parquet classmethod below."""
            ...

        @classmethod
        def read_json(cls, source: Any, schema: type[SchemaT], **kwargs: Any) -> PolarsFrame[SchemaT]:
            """Type stub matching the runtime read_json classmethod below."""
            ...

        @classmethod
        def read_excel(cls, source: Any, schema: type[SchemaT], **kwargs: Any) -> PolarsFrame[SchemaT]:
            """Type stub matching the runtime read_excel classmethod below."""
            ...

else:

    class PolarsFrame(Generic[SchemaT]):
        """
        Type marker for schema-annotated polars DataFrames.

        This is a type-only construct - at runtime, PolarsFrame[Schema] returns
        Annotated[pl.DataFrame, Schema], meaning the actual value is a plain
        pl.DataFrame with full polars functionality.

        The typedframes checker parses the Annotated metadata to validate
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

        def __class_getitem__(cls, schema: type[SchemaT]) -> Any:
            """
            Create an Annotated type combining pl.DataFrame with schema metadata.

            Args:
                schema: A BaseSchema subclass defining the DataFrame structure.

            Returns:
                Annotated[pl.DataFrame, schema] for type checking.

            """
            import polars as pl

            warnings.warn(_DEPRECATION_MESSAGE, DeprecationWarning, stacklevel=2)
            return Annotated[pl.DataFrame, schema]

        @classmethod
        def read_csv(cls, source: Any, schema: type[SchemaT], **kwargs: Any) -> Any:
            """
            Read a CSV file into a polars DataFrame.

            The schema parameter is for typing and static checking only — it is not
            validated or attached to the DataFrame at runtime. This method is a
            pass-through to ``pl.read_csv``.

            Args:
                source: File path or buffer to read from.
                schema: Schema class describing the expected DataFrame structure.
                    Used for static analysis only; ignored at runtime.
                **kwargs: Additional arguments passed to ``pl.read_csv``.

            Returns:
                A plain polars DataFrame. Annotate the result as ``PolarsFrame[Schema]``.

            """
            import polars as pl

            warnings.warn(_DEPRECATION_MESSAGE, DeprecationWarning, stacklevel=2)
            return pl.read_csv(source, **kwargs)

        @classmethod
        def read_parquet(cls, source: Any, schema: type[SchemaT], **kwargs: Any) -> Any:
            """
            Read a Parquet file into a polars DataFrame.

            The schema parameter is for typing and static checking only — it is not
            validated or attached to the DataFrame at runtime. This method is a
            pass-through to ``pl.read_parquet``.

            Args:
                source: File path or buffer to read from.
                schema: Schema class describing the expected DataFrame structure.
                    Used for static analysis only; ignored at runtime.
                **kwargs: Additional arguments passed to ``pl.read_parquet``.

            Returns:
                A plain polars DataFrame. Annotate the result as ``PolarsFrame[Schema]``.

            """
            import polars as pl

            warnings.warn(_DEPRECATION_MESSAGE, DeprecationWarning, stacklevel=2)
            return pl.read_parquet(source, **kwargs)

        @classmethod
        def read_json(cls, source: Any, schema: type[SchemaT], **kwargs: Any) -> Any:
            """
            Read a JSON file into a polars DataFrame.

            The schema parameter is for typing and static checking only — it is not
            validated or attached to the DataFrame at runtime. This method is a
            pass-through to ``pl.read_json``.

            Args:
                source: File path or buffer to read from.
                schema: Schema class describing the expected DataFrame structure.
                    Used for static analysis only; ignored at runtime.
                **kwargs: Additional arguments passed to ``pl.read_json``.

            Returns:
                A plain polars DataFrame. Annotate the result as ``PolarsFrame[Schema]``.

            """
            import polars as pl

            warnings.warn(_DEPRECATION_MESSAGE, DeprecationWarning, stacklevel=2)
            return pl.read_json(source, **kwargs)

        @classmethod
        def read_excel(cls, source: Any, schema: type[SchemaT], **kwargs: Any) -> Any:
            """
            Read an Excel file into a polars DataFrame.

            The schema parameter is for typing and static checking only — it is not
            validated or attached to the DataFrame at runtime. This method is a
            pass-through to ``pl.read_excel``.

            Args:
                source: File path or buffer to read from.
                schema: Schema class describing the expected DataFrame structure.
                    Used for static analysis only; ignored at runtime.
                **kwargs: Additional arguments passed to ``pl.read_excel``.

            Returns:
                A plain polars DataFrame. Annotate the result as ``PolarsFrame[Schema]``.

            """
            import polars as pl

            warnings.warn(_DEPRECATION_MESSAGE, DeprecationWarning, stacklevel=2)
            return pl.read_excel(source, **kwargs)


# Prefer ``Annotated[pl.DataFrame, Schema]`` for direct type annotations.
# ``PolarsFrame[Schema]`` is a convenience alias that expands to the same thing at runtime.
__all__ = ["PolarsFrame"]

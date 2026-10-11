"""Base schema class for defining DataFrame schemas."""

from __future__ import annotations

import re
from typing import TYPE_CHECKING, ClassVar

from .column import Column
from .column_group import ColumnGroup
from .column_set import ColumnSet

if TYPE_CHECKING:
    import pandas as pd
    import polars as pl


class SchemaMeta(type):
    """Metaclass for BaseSchema that enables class-level + operator and MI conflict detection."""

    def __new__(mcs, name: str, bases: tuple[type, ...], namespace: dict) -> SchemaMeta:
        """Create a new schema class, checking for column type conflicts across bases."""
        cls = super().__new__(mcs, name, bases, namespace)
        # Detect column type conflicts across multiple schema bases
        seen_columns: dict[str, tuple[type, str]] = {}
        for base in cls.__mro__:
            if base is cls or not isinstance(base, SchemaMeta):
                continue
            for attr_val in base.__dict__.values():
                if isinstance(attr_val, Column):
                    col_name = attr_val.column_name
                    if col_name in seen_columns:
                        existing_type, existing_source = seen_columns[col_name]
                        if attr_val.type != existing_type:
                            from .schema_algebra import SchemaConflictError

                            raise SchemaConflictError(
                                col_name, existing_type, existing_source, attr_val.type, base.__name__
                            )
                    else:
                        seen_columns[col_name] = (attr_val.type, base.__name__)
        return cls  # type: ignore[return-value]

    def __add__(cls, other: type[BaseSchema]) -> type[BaseSchema]:
        """Combine two schema classes using the + operator."""
        from .schema_algebra import combine_schemas

        return combine_schemas(cls, other)  # ty: ignore[invalid-argument-type]

    def __radd__(cls, other: type[BaseSchema]) -> type[BaseSchema]:
        """Support reverse addition for schema combination."""
        from .schema_algebra import combine_schemas

        return combine_schemas(other, cls)  # ty: ignore[invalid-argument-type]


def _collect_from_mro(cls: type, descriptor_type: type) -> dict:
    """Collect descriptors of a given type from the full MRO (child overrides parent)."""
    result: dict = {}
    for klass in reversed(cls.__mro__):
        result.update({name: val for name, val in klass.__dict__.items() if isinstance(val, descriptor_type)})
    return result


class BaseSchema(metaclass=SchemaMeta):
    r"""
    Backend-agnostic schema definition for DataFrame validation.

    Define your DataFrame schema once and use it for static analysis
    with both pandas and polars.

    Class Attributes:
        enforce_columns: If True, validate that all defined columns exist.
        allow_extra_columns: If True, allow columns not defined in schema.

    Example:
        class UserData(BaseSchema):
            user_id = Column(type=int)
            email = Column(type=str)
            scores = ColumnSet(members=r"score_\\d+", type=float, regex=True)

        # Use with type annotations for static analysis:
        df: Annotated[pd.DataFrame, UserData] = pd.read_csv("data.csv")
        df: Annotated[pl.DataFrame, UserData] = pl.read_csv("data.csv")

    """

    _column_map: ClassVar[dict[str, Column] | None] = None
    _column_set_map: ClassVar[dict[str, ColumnSet] | None] = None
    _column_group_map: ClassVar[dict[str, ColumnGroup] | None] = None

    enforce_columns: ClassVar[bool] = True
    allow_extra_columns: ClassVar[bool] = True

    @classmethod
    def columns(cls) -> dict[str, Column]:
        """Return mapping of attribute names to Column definitions, including inherited."""
        if "_column_map" not in cls.__dict__ or cls._column_map is None:
            cls._column_map = _collect_from_mro(cls, Column)
        return cls._column_map

    @classmethod
    def column_sets(cls) -> dict[str, ColumnSet]:
        """Return mapping of attribute names to ColumnSet definitions, including inherited."""
        if "_column_set_map" not in cls.__dict__ or cls._column_set_map is None:
            cls._column_set_map = _collect_from_mro(cls, ColumnSet)
        return cls._column_set_map

    @classmethod
    def column_groups(cls) -> dict[str, ColumnGroup]:
        """Return mapping of attribute names to ColumnGroup definitions, including inherited."""
        if "_column_group_map" not in cls.__dict__ or cls._column_group_map is None:
            cls._column_group_map = _collect_from_mro(cls, ColumnGroup)
        return cls._column_group_map

    @classmethod
    def all_column_names(cls) -> list[str]:
        """Return list of all explicitly defined column names (from Columns and non-regex ColumnSets)."""
        names: list[str] = [col.column_name for col in cls.columns().values()]

        for cs in cls.column_sets().values():
            if not cs.regex and isinstance(cs.members, list):
                names.extend(cs.members)

        return names

    @classmethod
    def validate_columns(cls, df_columns: list[str]) -> list[str]:
        """
        Validate DataFrame columns against schema.

        Returns list of error messages (empty if valid).
        """
        defined = set(cls.all_column_names())
        errors: list[str] = [
            f"Missing required column: {col.column_name}"
            for col in cls.columns().values()
            if col.column_name not in df_columns and cls.enforce_columns
        ]

        if not cls.allow_extra_columns:
            for col_name in df_columns:
                if col_name not in defined:
                    is_matched = False
                    for cs in cls.column_sets().values():
                        if cs.regex and isinstance(cs.members, list) and any(re.match(p, col_name) for p in cs.members):
                            is_matched = True
                            break
                    if not is_matched:
                        errors.append(f"Unexpected column: {col_name}")

        return errors

    @classmethod
    def from_pandas(cls, df: pd.DataFrame) -> pd.DataFrame:
        """
        Validate and return a pandas DataFrame with schema metadata.

        This is a simple passthrough for use with type annotations.
        The linter performs static validation; this provides runtime metadata.
        """
        return df

    @classmethod
    def from_polars(cls, df: pl.DataFrame) -> pl.DataFrame:
        """
        Validate and return a polars DataFrame with schema metadata.

        This is a simple passthrough for use with type annotations.
        The linter performs static validation; this provides runtime metadata.
        """
        return df

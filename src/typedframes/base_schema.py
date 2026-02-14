"""Base schema class for defining DataFrame schemas."""

from __future__ import annotations

import re
from collections import defaultdict
from typing import TYPE_CHECKING, ClassVar

from .column import Column
from .column_group import ColumnGroup
from .column_group_error import ColumnGroupError
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
        enforce_types: If True, enforce column types during I/O operations.
        allow_extra_columns: If True, allow columns not defined in schema.
        greedy_column_sets: If True, allow columns to match multiple ColumnSets.

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
    enforce_types: ClassVar[bool] = True
    allow_extra_columns: ClassVar[bool] = True
    greedy_column_sets: ClassVar[bool] = False

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
    def _build_key_column_map(cls) -> dict[str, Column]:
        """Build mapping of column keys to Column objects."""
        key_column_map: dict[str, Column] = {}
        for col in cls.columns().values():
            key_column_map[col.column_name] = col
        return key_column_map

    @classmethod
    def _match_column_to_set(
        cls,
        col_name: str,
        cs: ColumnSet,
        *,
        consumed: bool,
        greedy: bool,
        current_match: Column | ColumnSet | None,
    ) -> bool:
        """Check if column matches a ColumnSet. Returns True if matched."""
        if not isinstance(cs.members, list):
            return False

        matches = any(re.match(pattern, col_name) for pattern in cs.members) if cs.regex else col_name in cs.members

        if matches and consumed and not greedy:
            raise ColumnGroupError(col_name, current_match, cs)

        return matches and not consumed

    @classmethod
    def compute_column_map(
        cls,
        dataframe_columns: list[str],
        *,
        greedy: bool | None = None,
    ) -> tuple[dict[str, type], dict[str, list[str]]]:
        """
        Compute column type map and ColumnSet consumption from actual DataFrame columns.

        Args:
            dataframe_columns: List of column names from the DataFrame.
            greedy: Override greedy_column_sets setting.

        Returns:
            Tuple of (column_type_map, column_consumed_map) where:
            - column_type_map: Dict mapping column name to its type
            - column_consumed_map: Dict mapping ColumnSet name to list of matched columns

        Raises:
            ColumnGroupError: If a column matches multiple ColumnSets (when not greedy).

        """
        greedy = greedy if greedy is not None else cls.greedy_column_sets
        column_consumed_map: dict[str, list[str]] = defaultdict(list)
        key_column_map = cls._build_key_column_map()

        if not cls.column_sets():
            return {k: v.type for k, v in key_column_map.items()}, dict(column_consumed_map)

        column_bag: list[Column | ColumnSet | None] = [key_column_map.get(c) for c in dataframe_columns]
        consumed: list[bool] = [col is not None for col in column_bag]

        column_sets_list = list(cls.column_sets().values())
        for i, col_name in enumerate(dataframe_columns):
            for cs in column_sets_list:
                if cls._match_column_to_set(
                    col_name, cs, consumed=consumed[i], greedy=greedy, current_match=column_bag[i]
                ):
                    consumed[i] = True
                    column_bag[i] = cs
                    column_consumed_map[cs.name].append(col_name)

        result: dict[str, type] = {}
        for i, col_or_set in enumerate(column_bag):
            if col_or_set is not None:
                result[dataframe_columns[i]] = col_or_set.type

        return result, dict(column_consumed_map)

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

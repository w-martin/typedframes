"""Base schema class for defining DataFrame schemas."""

from __future__ import annotations

import re
from collections import defaultdict
from typing import TYPE_CHECKING, ClassVar

from .column import Column
from .column_alias_not_yet_defined_error import ColumnAliasNotYetDefinedError
from .column_group import ColumnGroup
from .column_group_error import ColumnGroupError
from .column_set import ColumnSet
from .column_set_members_not_yet_defined_error import ColumnSetMembersNotYetDefinedError
from .defined_later import DefinedLater

if TYPE_CHECKING:
    import pandas as pd
    import polars as pl


class BaseSchema:
    """
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
        """Return mapping of attribute names to Column definitions."""
        if cls._column_map is None:
            cls._column_map = {
                name: col
                for name, col in cls.__dict__.items()
                if isinstance(col, Column)
            }
        return cls._column_map

    @classmethod
    def column_sets(cls) -> dict[str, ColumnSet]:
        """Return mapping of attribute names to ColumnSet definitions."""
        if cls._column_set_map is None:
            cls._column_set_map = {
                name: cs
                for name, cs in cls.__dict__.items()
                if isinstance(cs, ColumnSet)
            }
        return cls._column_set_map

    @classmethod
    def column_groups(cls) -> dict[str, ColumnGroup]:
        """Return mapping of attribute names to ColumnGroup definitions."""
        if cls._column_group_map is None:
            cls._column_group_map = {
                name: cg
                for name, cg in cls.__dict__.items()
                if isinstance(cg, ColumnGroup)
            }
        return cls._column_group_map

    @classmethod
    def all_column_names(cls) -> list[str]:
        """Return list of all explicitly defined column names (from Columns and non-regex ColumnSets)."""
        names: list[str] = []

        for col in cls.columns().values():
            effective_name = col.alias if isinstance(col.alias, str) else col.name
            names.append(effective_name)

        for cs in cls.column_sets().values():
            if not cs.regex and isinstance(cs.members, list):
                names.extend(cs.members)

        return names

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
            ColumnAliasNotYetDefinedError: If a Column has alias=DefinedLater.
            ColumnSetMembersNotYetDefinedError: If a ColumnSet has members=DefinedLater.
            ColumnGroupError: If a column matches multiple ColumnSets (when not greedy).
        """
        greedy = greedy if greedy is not None else cls.greedy_column_sets
        column_consumed_map: dict[str, list[str]] = defaultdict(list)

        key_column_map: dict[str, Column] = {}
        for col in cls.columns().values():
            if col.alias is DefinedLater or isinstance(col.alias, DefinedLater):
                raise ColumnAliasNotYetDefinedError(col.name)
            key = col.alias if isinstance(col.alias, str) else col.name
            key_column_map[key] = col

        if not cls.column_sets():
            return {k: v.type for k, v in key_column_map.items()}, dict(column_consumed_map)

        column_bag: list[Column | ColumnSet | None] = [
            key_column_map.get(c) for c in dataframe_columns
        ]
        consumed: list[bool] = [col is not None for col in column_bag]

        for cs in cls.column_sets().values():
            if cs.members is DefinedLater or isinstance(cs.members, DefinedLater):
                raise ColumnSetMembersNotYetDefinedError(cs.name)

        exact_sets = [cs for cs in cls.column_sets().values() if not cs.regex]
        regex_sets = [cs for cs in cls.column_sets().values() if cs.regex]

        for i, col_name in enumerate(dataframe_columns):
            for cs in exact_sets:
                if isinstance(cs.members, list) and col_name in cs.members:
                    if consumed[i] and not greedy:
                        raise ColumnGroupError(col_name, column_bag[i], cs)
                    if not consumed[i]:
                        consumed[i] = True
                        column_bag[i] = cs
                        column_consumed_map[cs.name].append(col_name)

            for cs in regex_sets:
                if isinstance(cs.members, list):
                    if any(re.match(pattern, col_name) for pattern in cs.members):
                        if consumed[i] and not greedy:
                            raise ColumnGroupError(col_name, column_bag[i], cs)
                        if not consumed[i]:
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
        errors: list[str] = []
        defined = set(cls.all_column_names())

        for col in cls.columns().values():
            effective_name = col.alias if isinstance(col.alias, str) else col.name
            if effective_name not in df_columns and cls.enforce_columns:
                errors.append(f"Missing required column: {effective_name}")

        if not cls.allow_extra_columns:
            for col_name in df_columns:
                if col_name not in defined:
                    is_matched = False
                    for cs in cls.column_sets().values():
                        if cs.regex and isinstance(cs.members, list):
                            if any(re.match(p, col_name) for p in cs.members):
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

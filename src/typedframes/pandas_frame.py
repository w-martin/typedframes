"""PandasFrame - typed pandas DataFrame subclass with schema awareness."""

from __future__ import annotations

from typing import TYPE_CHECKING, Any, Generic, TypeVar

import pandas as pd

from .base_schema import BaseSchema
from .column import Column
from .column_alias_not_yet_defined_error import ColumnAliasNotYetDefinedError
from .column_group import ColumnGroup
from .column_set import ColumnSet
from .column_set_members_not_yet_defined_error import ColumnSetMembersNotYetDefinedError
from .defined_later import DefinedLater

if TYPE_CHECKING:
    from typing import Self

SchemaT = TypeVar("SchemaT", bound=BaseSchema)


class PandasFrame(pd.DataFrame, Generic[SchemaT]):
    """
    Pandas DataFrame subclass with schema-aware attribute access.

    Preserves all pandas functionality while adding schema-based
    column access via attribute names.

    Attributes:
        _schema_class: The schema class for this DataFrame.
        _column_consumed_map: Mapping of ColumnSet names to matched columns.

    Example:
        class UserData(BaseSchema):
            user_id = Column(type=int)
            email = Column(type=str)

        df = PandasFrame.from_schema(pd.read_csv("data.csv"), UserData)
        df.user_id  # Access column by schema attribute name
    """

    _metadata = ["_schema_class", "_column_consumed_map"]

    _schema_class: type[SchemaT] | None
    _column_consumed_map: dict[str, list[str]]

    def __init__(
        self,
        data: pd.DataFrame | dict | None = None,
        schema: type[SchemaT] | None = None,
        column_consumed_map: dict[str, list[str]] | None = None,
        **kwargs: Any,
    ) -> None:
        """
        Initialize a PandasFrame.

        Args:
            data: DataFrame data (passed to pd.DataFrame).
            schema: The schema class to associate with this DataFrame.
            column_consumed_map: Pre-computed ColumnSet consumption map.
            **kwargs: Additional arguments passed to pd.DataFrame.
        """
        super().__init__(data, **kwargs)
        self._schema_class = schema
        self._column_consumed_map = column_consumed_map or {}

    @classmethod
    def from_schema(
        cls,
        df: pd.DataFrame,
        schema: type[SchemaT],
        column_consumed_map: dict[str, list[str]] | None = None,
    ) -> PandasFrame[SchemaT]:
        """
        Create a PandasFrame from an existing DataFrame and schema.

        Args:
            df: Source pandas DataFrame.
            schema: Schema class to associate.
            column_consumed_map: Pre-computed ColumnSet consumption map.
                If not provided, will be computed from schema.

        Returns:
            PandasFrame with schema metadata.
        """
        if column_consumed_map is None:
            _, column_consumed_map = schema.compute_column_map(list(df.columns))

        return cls(df, schema=schema, column_consumed_map=column_consumed_map)

    def __getattr__(self, item: str) -> pd.Series | pd.DataFrame:
        """
        Access columns by schema attribute name.

        Schema-defined Columns, ColumnSets, and ColumnGroups are accessed
        before falling back to standard pandas attribute access.
        """
        if item.startswith("_") or item in {"_schema_class", "_column_consumed_map"}:
            return object.__getattribute__(self, item)  # type: ignore[return-value]

        try:
            schema = object.__getattribute__(self, "_schema_class")
            consumed_map = object.__getattribute__(self, "_column_consumed_map")
        except AttributeError:
            return super().__getattribute__(item)  # type: ignore[return-value]

        if schema is None:
            return super().__getattribute__(item)  # type: ignore[return-value]

        column_map = schema.columns()
        if item in column_map:
            col = column_map[item]
            if isinstance(col.alias, DefinedLater):
                raise ColumnAliasNotYetDefinedError(col.name)
            effective_name = col.alias if isinstance(col.alias, str) else col.name
            return self[effective_name]

        column_set_map = schema.column_sets()
        if item in column_set_map:
            cs = column_set_map[item]
            if isinstance(cs.members, DefinedLater):
                raise ColumnSetMembersNotYetDefinedError(cs.name)
            matched = consumed_map.get(cs.name, [])
            return self[matched]

        column_group_map = schema.column_groups()
        if item in column_group_map:
            group = column_group_map[item]
            col_names = group.get_column_names(consumed_map)
            return self[col_names]

        return super().__getattribute__(item)  # type: ignore[return-value]

    @property
    def _constructor(self) -> type[Self]:
        """Return constructor for slicing/operations to preserve PandasFrame type."""

        def constructor(data: Any, **kwargs: Any) -> PandasFrame[SchemaT]:
            return PandasFrame(
                data,
                schema=self._schema_class,
                column_consumed_map=self._column_consumed_map,
                **kwargs,
            )

        return constructor  # type: ignore[return-value]

    @property
    def _constructor_sliced(self) -> type[pd.Series]:
        """Return Series constructor for column access."""
        return pd.Series

    def to_pandas(self) -> pd.DataFrame:
        """Convert to plain pandas DataFrame (drops schema metadata)."""
        return pd.DataFrame(self)

    @property
    def schema(self) -> type[SchemaT] | None:
        """Return the schema class associated with this DataFrame."""
        return self._schema_class

"""PandasFrame - typed pandas DataFrame subclass with schema awareness."""

from __future__ import annotations

from typing import TYPE_CHECKING, Any, ClassVar, Generic, TypeVar

import pandas as pd

from .base_schema import BaseSchema
from .column_alias_not_yet_defined_error import ColumnAliasNotYetDefinedError
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

    _metadata: ClassVar[list[str]] = ["_schema_class", "_column_consumed_map"]

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

    def _lookup_schema_item(
        self, item: str, schema: type[SchemaT], consumed_map: dict[str, list[str]]
    ) -> pd.Series | pd.DataFrame | None:
        """Look up item in schema columns, column sets, and column groups."""
        # Check columns
        if item in schema.columns():
            col = schema.columns()[item]
            if col.alias is DefinedLater:
                raise ColumnAliasNotYetDefinedError(col.name)
            effective_name = col.alias if isinstance(col.alias, str) else col.name
            return self[effective_name]

        # Check column sets
        if item in schema.column_sets():
            cs = schema.column_sets()[item]
            if cs.members is DefinedLater:
                raise ColumnSetMembersNotYetDefinedError(cs.name)
            return self[consumed_map.get(cs.name, [])]

        # Check column groups
        if item in schema.column_groups():
            group = schema.column_groups()[item]
            return self[group.get_column_names(consumed_map)]

        return None

    def __getattr__(  # ty: ignore[override-of-final-method, invalid-method-override]
        self, item: str
    ) -> pd.Series | pd.DataFrame:
        """
        Access columns by schema attribute name.

        Schema-defined Columns, ColumnSets, and ColumnGroups are accessed
        before falling back to standard pandas attribute access.
        """
        if item.startswith("_") or item in {"_schema_class", "_column_consumed_map"}:
            return object.__getattribute__(self, item)

        try:
            schema = object.__getattribute__(self, "_schema_class")
            consumed_map = object.__getattribute__(self, "_column_consumed_map")
        except AttributeError:
            schema = None
            consumed_map = {}

        if schema is not None:
            result = self._lookup_schema_item(item, schema, consumed_map)
            if result is not None:
                return result

        return super().__getattribute__(item)

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

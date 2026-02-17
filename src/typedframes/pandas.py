"""PandasFrame - typed pandas DataFrame subclass with schema awareness."""

from __future__ import annotations

from typing import TYPE_CHECKING, Any, ClassVar, Generic, TypeVar, overload

import pandas as pd

from .base_schema import BaseSchema
from .column import Column
from .column_group import ColumnGroup
from .column_set import ColumnSet

if TYPE_CHECKING:
    from typing import Self

SchemaT = TypeVar("SchemaT", bound=BaseSchema)


class PandasFrame(pd.DataFrame, Generic[SchemaT]):
    """
    Pandas DataFrame subclass with schema-aware column access.

    Preserves all pandas functionality while adding schema-based
    column access via ``__getitem__`` overloads.

    Attributes:
        _schema_class: The schema class for this DataFrame.
        _column_consumed_map: Mapping of ColumnSet names to matched columns.

    Example:
        class UserData(BaseSchema):
            user_id = Column(type=int)
            email = Column(type=str)

        df = PandasFrame.from_schema(pd.read_csv("data.csv"), UserData)
        df[UserData.user_id]  # pd.Series via Column descriptor
        df["email"]           # pd.Series via string key (standard pandas)

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

        return cls(df, schema=schema, column_consumed_map=column_consumed_map)  # ty: ignore[no-matching-overload]

    @classmethod
    def read_csv(cls, filepath_or_buffer: Any, schema: type[SchemaT], **kwargs: Any) -> PandasFrame[SchemaT]:
        """
        Read a CSV file and create a schema-aware PandasFrame.

        Args:
            filepath_or_buffer: File path or buffer to read from.
            schema: Schema class to associate with the DataFrame.
            **kwargs: Additional arguments passed to ``pd.read_csv``.

        Returns:
            PandasFrame with schema metadata.

        """
        return cls.from_schema(pd.read_csv(filepath_or_buffer, **kwargs), schema)

    @classmethod
    def read_parquet(cls, path: Any, schema: type[SchemaT], **kwargs: Any) -> PandasFrame[SchemaT]:
        """
        Read a Parquet file and create a schema-aware PandasFrame.

        Args:
            path: File path to read from.
            schema: Schema class to associate with the DataFrame.
            **kwargs: Additional arguments passed to ``pd.read_parquet``.

        Returns:
            PandasFrame with schema metadata.

        """
        return cls.from_schema(pd.read_parquet(path, **kwargs), schema)

    @classmethod
    def read_json(cls, path_or_buf: Any, schema: type[SchemaT], **kwargs: Any) -> PandasFrame[SchemaT]:
        """
        Read a JSON file and create a schema-aware PandasFrame.

        Args:
            path_or_buf: File path or buffer to read from.
            schema: Schema class to associate with the DataFrame.
            **kwargs: Additional arguments passed to ``pd.read_json``.

        Returns:
            PandasFrame with schema metadata.

        """
        return cls.from_schema(pd.read_json(path_or_buf, **kwargs), schema)

    @classmethod
    def read_excel(cls, io: Any, schema: type[SchemaT], **kwargs: Any) -> PandasFrame[SchemaT]:
        """
        Read an Excel file and create a schema-aware PandasFrame.

        Args:
            io: File path or ExcelFile object to read from.
            schema: Schema class to associate with the DataFrame.
            **kwargs: Additional arguments passed to ``pd.read_excel``.

        Returns:
            PandasFrame with schema metadata.

        """
        return cls.from_schema(pd.read_excel(io, **kwargs), schema)

    if TYPE_CHECKING:

        @overload
        def __getitem__(self, key: Column) -> pd.Series: ...

        @overload
        def __getitem__(self, key: ColumnSet) -> pd.DataFrame: ...

        @overload
        def __getitem__(self, key: ColumnGroup) -> pd.DataFrame: ...

        @overload
        def __getitem__(self, key: str) -> pd.Series: ...

        @overload
        def __getitem__(self, key: list[str]) -> pd.DataFrame: ...

        @overload
        def __getitem__(self, key: pd.Series) -> PandasFrame[SchemaT]: ...

    def __getitem__(  # ty: ignore[invalid-method-override]
            self,
            key: Column | ColumnSet | ColumnGroup | str | list[str] | pd.Series,
    ) -> pd.Series | pd.DataFrame:
        """
        Access columns by schema descriptor, string key, or boolean mask.

        Supports Column, ColumnSet, ColumnGroup descriptors from the schema,
        as well as standard pandas string and list-of-string access.
        """
        if isinstance(key, Column):
            return super().__getitem__(key.column_name)
        if isinstance(key, ColumnSet):
            matched = self._column_consumed_map.get(key.name, [])
            return super().__getitem__(matched)
        if isinstance(key, ColumnGroup):
            names = key.get_column_names(self._column_consumed_map)
            return super().__getitem__(names)
        return super().__getitem__(key)

    def _resolve_by(
            self,
            by: Column | ColumnSet | ColumnGroup | str | list[Any] | None,
    ) -> str | list[str] | None:
        """Convert schema descriptors in ``by`` to column name strings."""
        if by is None:
            return None
        if isinstance(by, Column):
            return by.column_name
        if isinstance(by, ColumnSet):
            return self._column_consumed_map.get(by.name, [])
        if isinstance(by, ColumnGroup):
            return by.get_column_names(self._column_consumed_map)
        if isinstance(by, list):
            resolved: list[str] = []
            for item in by:
                result = self._resolve_by(item)
                if isinstance(result, list):
                    resolved.extend(result)
                else:
                    resolved.append(result)  # ty: ignore[invalid-argument-type]
            return resolved
        return by

    def groupby(self, by: Any = None, **kwargs: Any) -> Any:  # ty: ignore[invalid-method-override]
        """Group by schema descriptors, strings, or mixed lists.

        Accepts ``Column``, ``ColumnSet``, ``ColumnGroup`` descriptors
        in addition to standard pandas groupby arguments.

        Args:
            by: Column(s) to group by. Accepts schema descriptors.
            **kwargs: Additional arguments passed to ``pd.DataFrame.groupby``.

        Returns:
            DataFrameGroupBy object.

        """
        return super().groupby(self._resolve_by(by), **kwargs)

    @property
    def _constructor(self) -> type[Self]:
        """Return constructor for slicing/operations to preserve PandasFrame type."""

        def constructor(data: Any, **kwargs: Any) -> PandasFrame[SchemaT]:
            return PandasFrame(  # ty: ignore[no-matching-overload]
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

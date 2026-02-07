"""ColumnSet descriptor for DataFrame schemas."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

from .defined_later import DefinedLater

if TYPE_CHECKING:
    import polars as pl


@dataclass
class ColumnSet:
    r"""
    Represents a set of columns matching a pattern or explicit list.

    Used for grouping related columns that share a common type,
    such as time series data or multi-dimensional measurements.

    Attributes:
        members: List of column names, or a regex pattern if regex=True.
            Use DefinedLater if members will be set at runtime.
        type: The Python type shared by all columns in the set.
        regex: If True, members is treated as a regex pattern for matching column names.
        description: Human-readable description of the column set's purpose.

    Example:
        class SensorData(BaseSchema):
            # Explicit member list
            temperatures = ColumnSet(members=["temp_1", "temp_2", "temp_3"], type=float)

            # Regex pattern matching
            pressures = ColumnSet(members=r"pressure_\\d+", type=float, regex=True)

    """

    members: list[str] | str | type[DefinedLater]  # ty: ignore[invalid-type-form]
    type: type = Any
    regex: bool = False
    description: str = ""
    name: str = field(default="", init=False)

    def __post_init__(self) -> None:
        """Normalize regex member to a list."""
        if self.regex and isinstance(self.members, str):
            self.members = [self.members]

    def __set_name__(self, owner: type, name: str) -> None:
        """Set the name attribute from the class attribute name."""
        self.name = name

    def cols(self, matched_columns: list[str] | None = None) -> list[pl.Expr]:
        """
        Return polars column expressions for all columns in this set.

        Args:
            matched_columns: Optional list of column names that matched this set.
                If not provided, uses the explicit members list (not applicable for regex).

        Returns:
            List of polars column expressions.

        Example:
            df.select(SensorSchema.temperatures.cols())

        """
        import polars as pl

        if matched_columns is not None:
            return [pl.col(c) for c in matched_columns]

        if self.members is DefinedLater or self.regex:
            msg = "Cannot get column expressions for regex or DefinedLater members without matched_columns"
            raise ValueError(msg)

        return [pl.col(c) for c in self.members]

"""ColumnSet descriptor for DataFrame schemas."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    import polars as pl


@dataclass
class ColumnSet:
    r"""
    Represents a set of columns matching a pattern or explicit list.

    Used for grouping related columns that share a common type,
    such as time series data or multi-dimensional measurements.

    Attributes:
        members: List of column names (or a single name as a string, which is normalized to a
            single-element list). When regex=True, each string is treated as a regex pattern.
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

    members: list[str]
    type: type = Any
    regex: bool = False
    description: str = ""
    name: str = field(default="", init=False)

    def __post_init__(self) -> None:
        """Normalize members to a list."""
        if isinstance(self.members, str):
            self.members = [self.members]

    def __set_name__(self, owner: type, name: str) -> None:
        """Set the name attribute from the class attribute name."""
        self.name = name

    @property
    def s(self) -> list[str]:
        """
        Return the column names as a list of strings for native pandas subscript access.

        For non-regex ColumnSets, returns the explicit member names.  For regex
        ColumnSets, raises ``ValueError`` because the matched column names are only
        known at runtime (the deprecated ``PandasFrame.from_schema()`` resolves
        these; there is currently no ``Annotated[...]``-based equivalent since that
        path has no runtime component to introspect actual columns against).

        Example:
            df[SensorSchema.temperatures.s]  # pandas — works for non-regex ColumnSets

        """
        if self.regex:
            msg = (
                "Cannot get column names for regex ColumnSet without matched columns. "
                "Use the deprecated PandasFrame.from_schema() to resolve regex "
                "patterns at runtime."
            )
            raise ValueError(msg)
        return list(self.members)

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
        try:
            import polars as pl
        except ImportError:
            from .missing_dependency_error import MissingDependencyError

            package = "polars"
            raise MissingDependencyError(package, "ColumnSet.cols") from None

        if matched_columns is not None:
            return [pl.col(c) for c in matched_columns]

        if self.regex:
            msg = "Cannot get column expressions for regex members without matched_columns"
            raise ValueError(msg)

        return [pl.col(c) for c in self.members]

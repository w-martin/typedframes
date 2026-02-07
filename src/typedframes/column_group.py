"""ColumnGroup descriptor for DataFrame schemas."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import TYPE_CHECKING

from .column import Column
from .column_set import ColumnSet

if TYPE_CHECKING:
    import polars as pl


@dataclass
class ColumnGroup:
    r"""
    Groups multiple Columns and ColumnSets for convenient access.

    Useful for organizing related columns that span multiple Column/ColumnSet
    definitions, such as grouping all measurement columns together.

    Attributes:
        members: List of Column, ColumnSet, or nested ColumnGroup objects.
        description: Human-readable description of the group's purpose.

    Example:
        class SensorData(BaseSchema):
            timestamp = Column(type=str)
            temperatures = ColumnSet(members=r"temp_\\d+", type=float, regex=True)
            pressures = ColumnSet(members=r"pressure_\\d+", type=float, regex=True)

            # Group for convenient access to all sensor data
            all_sensors = ColumnGroup(members=[temperatures, pressures])

    """

    members: list[Column | ColumnSet | ColumnGroup]
    description: str = ""
    name: str = field(default="", init=False)

    def __set_name__(self, owner: type, name: str) -> None:
        """Set the name attribute from the class attribute name."""
        self.name = name

    def get_column_names(self, consumed_map: dict[str, list[str]] | None = None) -> list[str]:
        """
        Get all column names in this group.

        Args:
            consumed_map: Mapping of ColumnSet names to matched column names.
                Required for ColumnSets with regex patterns.

        Returns:
            List of column names.

        """
        consumed_map = consumed_map or {}
        result: list[str] = []

        for member in self.members:
            if isinstance(member, Column):
                effective_name = member.alias if isinstance(member.alias, str) else member.name
                result.append(effective_name)
            elif isinstance(member, ColumnSet):
                if member.name in consumed_map:
                    result.extend(consumed_map[member.name])
                elif isinstance(member.members, list):
                    result.extend(member.members)
            elif isinstance(member, ColumnGroup):
                result.extend(member.get_column_names(consumed_map))

        return result

    def cols(self, consumed_map: dict[str, list[str]] | None = None) -> list[pl.Expr]:
        """
        Return polars column expressions for all columns in this group.

        Args:
            consumed_map: Mapping of ColumnSet names to matched column names.
                Required for ColumnSets with regex patterns.

        Returns:
            List of polars column expressions.

        Example:
            df.select(SensorSchema.all_sensors.cols())

        """
        import polars as pl

        names = self.get_column_names(consumed_map)
        return [pl.col(n) for n in names]

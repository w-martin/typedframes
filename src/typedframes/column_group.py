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

    @property
    def s(self) -> list[str]:
        """
        Return all column names in this group as a list of strings.

        Groups containing regex ``ColumnSet`` members raise ``ValueError`` because
        matched column names are only known at runtime.

        Returns:
            Flat list of column name strings for all non-regex members.

        Example:
            df[SensorSchema.all_sensors.s]  # pandas — works for non-regex groups

        """
        result: list[str] = []
        for member in self.members:
            if isinstance(member, Column):
                result.append(member.alias if isinstance(member.alias, str) else member.name)
            elif isinstance(member, ColumnSet):
                if member.regex:
                    msg = f"Cannot resolve regex ColumnSet '{member.name}': its matches are only known at runtime."
                    raise ValueError(msg)
                result.extend(member.members)
            elif isinstance(member, ColumnGroup):
                result.extend(member.s)
        return result

    def cols(self) -> list[pl.Expr]:
        """
        Return polars column expressions for all columns in this group.

        Groups containing regex ``ColumnSet`` members raise ``ValueError``, as for ``.s``.

        Returns:
            List of polars column expressions.

        Example:
            df.select(SensorSchema.all_sensors.cols())

        """
        try:
            import polars as pl
        except ImportError:
            from .missing_dependency_error import MissingDependencyError

            package = "polars"
            raise MissingDependencyError(package, "ColumnGroup.cols") from None

        return [pl.col(n) for n in self.s]

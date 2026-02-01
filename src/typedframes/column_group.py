from dataclasses import dataclass
from typing import Any

from .column import Column
from .column_set import ColumnSet


@dataclass
class ColumnGroup:
    """A group of columns that can be used to define a column set."""

    members: list[Column | ColumnSet]

    def __set_name__(self, _: Any, name: str) -> None:
        """Set the name attribute of the ColumnGroup instance."""
        self.name = name

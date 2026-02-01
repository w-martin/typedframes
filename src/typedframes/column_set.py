from dataclasses import dataclass
from typing import Any

from .defined_later import DefinedLater


@dataclass
class ColumnSet:
    """
    Represents a set of columns with optional type and regex matching capabilities.

    This class is used to define a set of columns for further processing, with an
    optional ability to specify the data type and enable matching based on regular
    expressions.
    """

    members: list[str] | DefinedLater | DefinedLater.__class__  # list of columns matched to this set
    type: type = Any  # dtype applied to this set
    regex: bool = False  # enables matching members by regex

    def __set_name__(self, _: Any, name: str) -> None:
        """Set the name attribute of the ColumnSet instance."""
        self.name = name

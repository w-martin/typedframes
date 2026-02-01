from dataclasses import dataclass
from typing import Any

from .defined_later import DefinedLater


@dataclass
class Column:
    """Represents a column in a DataFrame."""

    type: type = Any
    alias: str | DefinedLater | DefinedLater.__class__ | None = None

    def __set_name__(self, _: Any, name: str) -> None:
        """Set the name attribute of the Column instance."""
        self.name = name

    @property
    def column_name(self) -> str | DefinedLater | DefinedLater.__class__ | None:
        """Return the column name."""
        return self.alias or self.name

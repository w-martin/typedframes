"""Error for column conflicts between groups."""

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from .column import Column
    from .column_set import ColumnSet


class ColumnGroupError(Exception):
    """Exception raised when a column is matched by multiple ColumnSets or Columns."""

    def __init__(
        self,
        column: str,
        first_match: "Column | ColumnSet | None",
        second_match: "ColumnSet",
    ) -> None:
        """
        Initialize the error.

        Args:
            column: The name of the column that matched multiple groups.
            first_match: The first Column or ColumnSet that matched.
            second_match: The second ColumnSet that matched.

        """
        self._column = column
        self._first_match = first_match
        self._second_match = second_match
        super().__init__(str(self))

    def __str__(self) -> str:
        """Return a string representation of the error message."""
        first_name = getattr(self._first_match, "name", str(self._first_match))
        second_name = self._second_match.name
        return f"Column '{self._column}' matched by both '{first_name}' and '{second_name}'."

    @property
    def column(self) -> str:
        """Return the name of the conflicting column."""
        return self._column

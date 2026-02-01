from .column import Column
from .column_set import ColumnSet


class ColumnGroupError(Exception):
    """Exception raised when a column is matched by members of two different column groups."""

    def __init__(self, column: str, first_column_group: ColumnSet | Column, second_column_group: ColumnSet) -> None:
        self._column = column
        self._first_column_group = first_column_group
        self._second_column_group = second_column_group
        super().__init__()

    def __str__(self) -> str:
        """Return a string representation of the error message."""
        return (
            f"Error. Column {self._column} is matched by members "
            f"in {self._first_column_group} and {self._second_column_group}."
        )

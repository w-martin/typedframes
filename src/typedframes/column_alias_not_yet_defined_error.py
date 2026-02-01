from .column import Column


class ColumnAliasNotYetDefinedError(Exception):
    """Exception raised when a Column alias is used before it is defined."""

    def __init__(self, column: Column) -> None:
        self._column = column

    def __str__(self) -> str:
        """Return a string representation of the error message."""
        return f"Error. Column with name {self._column.name} was used with an alias that is not defined."

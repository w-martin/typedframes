"""Error for undefined column aliases."""


class ColumnAliasNotYetDefinedError(Exception):
    """Exception raised when a Column alias is accessed before it is defined."""

    def __init__(self, column_name: str) -> None:
        """
        Initialize the error.

        Args:
            column_name: The name of the column with undefined alias.

        """
        self._column_name = column_name
        super().__init__(str(self))

    def __str__(self) -> str:
        """Return a string representation of the error message."""
        return f"Column '{self._column_name}' has alias=DefinedLater but was accessed before alias was set."

    @property
    def column_name(self) -> str:
        """Return the name of the column with undefined alias."""
        return self._column_name

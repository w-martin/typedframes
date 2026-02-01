"""Error for undefined ColumnSet members."""


class ColumnSetMembersNotYetDefinedError(Exception):
    """Exception raised when a ColumnSet is accessed before its members are defined."""

    def __init__(self, column_set_name: str) -> None:
        """
        Initialize the error.

        Args:
            column_set_name: The name of the ColumnSet with undefined members.
        """
        self._column_set_name = column_set_name
        super().__init__(str(self))

    def __str__(self) -> str:
        """Return a string representation of the error message."""
        return f"ColumnSet '{self._column_set_name}' has members=DefinedLater but was accessed before members were set."

    @property
    def column_set_name(self) -> str:
        """Return the name of the ColumnSet with undefined members."""
        return self._column_set_name

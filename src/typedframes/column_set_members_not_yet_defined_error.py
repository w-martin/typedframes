from .column_set import ColumnSet


class ColumnSetMembersNotYetDefinedError(Exception):
    """Exception raised when a ColumnSet is used with an undefined member list."""

    def __init__(self, column_set: ColumnSet) -> None:
        self._column_set = column_set

    def __str__(self) -> str:
        """
        Return a string representation of the error message.

        For situations where a ColumnSet is used with an undefined member list.
        """
        return f"Error. ColumnSet with name {self._column_set.name} was used with a member list that is not defined."

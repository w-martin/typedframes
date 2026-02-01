class DefinedLater:
    """
    Denotes that a Column alias or ColumnSet member list will be defined dynamically at runtime.

    This class is used to signal that an alias or list of members will be provided later,
    typically during runtime. If any read operation is attempted before the placeholder
    is replaced with the actual value, a ColumnAliasNotYetDefinedException or
    ColumnSetMembersNotYetDefinedException will be raised.
    """

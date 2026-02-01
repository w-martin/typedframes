"""
A module providing core classes and errors for schema and column management.

This module includes foundational classes and exceptions used to define and
manage schemas, columns, column sets, and column groups. The classes serve as
building blocks for defining data structures in a tabular format, while the
errors help handle and manage inconsistencies or unresolved definitions.

Classes:
    BaseSchema:
        Represents the base schema for tabular data structure definitions.

    Column:
        Represents a single column in a tabular data structure.

    ColumnSet:
        Represents a set of columns, providing grouping and collective
        operations on them.

    ColumnGroup:
        Represents a logical grouping of columns that can be managed together.

    DefinedLater:
        A placeholder class for entities to be defined later in the schema or
        processing.

Exceptions:
    ColumnGroupError:
        Raised when there is an error in handling or processing a column group.

    ColumnAliasNotYetDefinedError:
        Raised when a column alias is referenced before being defined.

    ColumnSetMembersNotYetDefinedError:
        Raised when one or more members of a column set are referenced
        before being defined.
"""
from .base_schema import BaseSchema as BaseSchema
from .column import Column as Column
from .column_set import ColumnSet as ColumnSet
from .column_group import ColumnGroup as ColumnGroup
from .column_group_error import ColumnGroupError as ColumnGroupError
from .defined_later import DefinedLater as DefinedLater
from .column_alias_not_yet_defined_error import ColumnAliasNotYetDefinedError as ColumnAliasNotYetDefinedError
from .column_set_members_not_yet_defined_error import ColumnSetMembersNotYetDefinedError as ColumnSetMembersNotYetDefinedError

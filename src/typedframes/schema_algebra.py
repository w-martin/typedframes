"""Schema algebra operations for combining and transforming schemas."""

from __future__ import annotations

from typing import TYPE_CHECKING

from .column import Column
from .column_set import ColumnSet

if TYPE_CHECKING:
    from .base_schema import BaseSchema


class SchemaConflictError(TypeError):
    """Raised when combining schemas with conflicting column types."""

    def __init__(self, column_name: str, type_a: type, schema_a: str, type_b: type, schema_b: str) -> None:
        """Initialize with details about the conflicting column and types."""
        self.column_name = column_name
        self.type_a = type_a
        self.type_b = type_b
        super().__init__(
            f"Column '{column_name}' has conflicting types: "
            f"{type_a.__name__} ({schema_a}) vs {type_b.__name__} ({schema_b})"
        )


def combine_schemas(
        schema_a: type[BaseSchema],
        schema_b: type[BaseSchema],
        name: str | None = None,
) -> type[BaseSchema]:
    """
    Combine two schemas into a new schema with all columns from both.

    This is the functional equivalent of SchemaA + SchemaB.

    Args:
        schema_a: First schema class.
        schema_b: Second schema class.
        name: Optional name for the combined schema. Defaults to "SchemaA_SchemaB".

    Returns:
        A new schema class with columns from both schemas.

    Raises:
        SchemaConflictError: If both schemas have a column with the same name but different types.

    """
    from .base_schema import BaseSchema

    combined_name = name or f"{schema_a.__name__}_{schema_b.__name__}"

    attrs: dict = {}

    for col_name, col in schema_a.columns().items():
        attrs[col_name] = Column(
            type=col.type,
            alias=col.alias,
            nullable=col.nullable,
            description=col.description,
        )

    for col_name, col in schema_b.columns().items():
        if col_name in attrs:
            existing = attrs[col_name]
            if existing.type != col.type:
                raise SchemaConflictError(col_name, existing.type, schema_a.__name__, col.type, schema_b.__name__)
        else:
            attrs[col_name] = Column(
                type=col.type,
                alias=col.alias,
                nullable=col.nullable,
                description=col.description,
            )

    for cs_name, cs in schema_a.column_sets().items():
        attrs[cs_name] = ColumnSet(
            members=cs.members.copy() if isinstance(cs.members, list) else cs.members,
            type=cs.type,
            regex=cs.regex,
            description=cs.description,
        )

    for cs_name, cs in schema_b.column_sets().items():
        if cs_name not in attrs:
            attrs[cs_name] = ColumnSet(
                members=cs.members.copy() if isinstance(cs.members, list) else cs.members,
                type=cs.type,
                regex=cs.regex,
                description=cs.description,
            )

    # Create the new schema class
    combined_schema = type(combined_name, (BaseSchema,), attrs)

    # Set names on Column/ColumnSet descriptors
    for attr_name, attr_value in attrs.items():
        attr_value.__set_name__(combined_schema, attr_name)

    return combined_schema

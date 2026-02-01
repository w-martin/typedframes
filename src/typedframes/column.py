"""Column descriptor for DataFrame schemas."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

from .defined_later import DefinedLater

if TYPE_CHECKING:
    import polars as pl


@dataclass
class Column:
    """
    Represents a single column in a DataFrame schema.

    Attributes:
        type: The Python type of the column (e.g., int, str, float).
        alias: Optional alternative name for the column in the actual DataFrame.
            Use DefinedLater if the alias will be set at runtime.
        nullable: Whether the column allows null values.
        description: Human-readable description of the column's purpose.

    Example:
        class UserData(BaseSchema):
            user_id = Column(type=int)
            email = Column(type=str, alias="user_email")
            age = Column(type=int, nullable=True, description="User's age in years")
    """

    type: type = Any
    alias: str | DefinedLater | None = None
    nullable: bool = False
    description: str = ""
    name: str = field(default="", init=False)

    def __set_name__(self, owner: Any, name: str) -> None:
        """Set the name attribute from the class attribute name."""
        self.name = name

    @property
    def column_name(self) -> str | DefinedLater | None:
        """Return the effective column name (alias if set, otherwise attribute name)."""
        return self.alias or self.name

    @property
    def col(self) -> pl.Expr:
        """
        Return a polars column expression for this column.

        Useful for building polars queries with schema column references.

        Example:
            df.filter(UserSchema.age.col > 18)
            df.select(UserSchema.email.col, UserSchema.user_id.col)
        """
        import polars as pl

        effective_name = self.alias if isinstance(self.alias, str) else self.name
        return pl.col(effective_name)

    def __str__(self) -> str:
        """Return the column name as a string for use in subscript access."""
        if isinstance(self.alias, str):
            return self.alias
        return self.name

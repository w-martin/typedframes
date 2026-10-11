"""Implementations that need Python 3.15 syntax. Only `compat` imports this, and only on 3.15 or later."""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from collections.abc import Iterable


def flatten[T](lists: Iterable[Iterable[T]]) -> list[T]:
    """Every item of every list, in order, as one list."""
    return [*items for items in lists]

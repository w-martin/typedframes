"""Helpers that behave the same on every supported Python, using newer syntax where there is some.

A module that uses syntax newer than the oldest supported Python can't even be parsed there, so each
such implementation lives in its own `pythonNNN` module and is imported only when the running version has
it. Each helper has a portable twin that runs everywhere else.
"""

from __future__ import annotations

from itertools import chain
from sys import version_info
from typing import TYPE_CHECKING, TypeVar

if TYPE_CHECKING:
    from collections.abc import Iterable

T = TypeVar("T")


def flatten_portable(lists: Iterable[Iterable[T]]) -> list[T]:
    """Every item of every list, in order, as one list."""
    return list(chain.from_iterable(lists))


if version_info >= (3, 15):  # pragma: no cover
    from typedframes.python315 import flatten
else:  # pragma: no cover
    flatten = flatten_portable

"""Tests for the version-gated helpers."""

from importlib import import_module
from sys import version_info
from unittest import TestCase, skipUnless

from typedframes import compat

_INPUTS = [
    [],
    [[]],
    [[1, 2], [3], []],
    [(1, 2), (3,)],
    [[{"a": 1}], [{"b": 2}, {"c": 3}]],
    [["x"], ["y", "z"]],
]


class TestFlatten(TestCase):
    """One list from many, the same on every Python."""

    def test_should_flatten_lists_in_order_with_the_portable_version(self) -> None:
        """Test the version that runs on every supported Python."""
        # arrange / act / assert
        self.assertEqual([1, 2, 3, 4], compat.flatten_portable([[1, 2], [], [3], [4]]))

    def test_should_accept_any_iterable_of_iterables(self) -> None:
        """Test that generators and tuples flatten like lists."""
        # arrange
        lists = (list(range(n)) for n in range(4))

        # act
        result = compat.flatten(lists)

        # assert
        self.assertEqual([0, 0, 1, 0, 1, 2], result)

    def test_should_use_the_python_3_15_version_only_where_it_exists(self) -> None:
        """Test that the running version picks the implementation its syntax allows."""
        # arrange / act / assert
        if version_info >= (3, 15):
            self.assertEqual("typedframes.python315", compat.flatten.__module__)
        else:
            self.assertIs(compat.flatten_portable, compat.flatten)

    @skipUnless(version_info >= (3, 15), "comprehension unpacking needs Python 3.15")
    def test_should_flatten_exactly_like_the_portable_version_on_python_3_15(self) -> None:
        """Test that both versions agree on every input."""
        # arrange
        flatten = import_module("typedframes.python315").flatten

        for lists in _INPUTS:
            with self.subTest(lists=lists):
                # act / assert
                self.assertEqual(compat.flatten_portable(lists), flatten(lists))

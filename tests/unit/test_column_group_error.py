"""Unit tests for ColumnGroupError."""

import unittest

from typedframes import ColumnGroupError, ColumnSet


class TestColumnGroupError(unittest.TestCase):
    """Unit tests for ColumnGroupError."""

    def test_should_expose_column_property(self) -> None:
        """Test that the column property returns the conflicting column name."""
        # arrange
        cs1 = ColumnSet(members=["temp_1"], type=float)
        cs1.__set_name__(None, "temps")
        cs2 = ColumnSet(members=["temp_1"], type=float)
        cs2.__set_name__(None, "readings")
        sut = ColumnGroupError("temp_1", cs1, cs2)

        # act
        result = sut.column

        # assert
        self.assertEqual(result, "temp_1")

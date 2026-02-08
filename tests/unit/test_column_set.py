"""Unit tests for ColumnSet class."""

import unittest

from typedframes import ColumnSet


class TestColumnSet(unittest.TestCase):
    """Unit tests for ColumnSet descriptor."""

    def test_should_create_column_set_with_members(self) -> None:
        """Test that ColumnSet can be created with explicit members."""
        # arrange/act
        sut = ColumnSet(members=["col1", "col2", "col3"], type=float)

        # assert
        self.assertEqual(sut.members, ["col1", "col2", "col3"])
        self.assertEqual(sut.type, float)

    def test_should_create_column_set_with_regex(self) -> None:
        """Test that ColumnSet can be created with regex pattern."""
        # arrange/act
        sut = ColumnSet(members=r"temp_\d+", type=float, regex=True)

        # assert
        self.assertEqual(sut.members, [r"temp_\d+"])  # Normalized to list
        self.assertTrue(sut.regex)

    def test_should_set_name_via_descriptor(self) -> None:
        """Test that __set_name__ sets the name attribute."""
        # arrange
        sut = ColumnSet(members=["a", "b"], type=int)

        # act
        sut.__set_name__(None, "metrics")

        # assert
        self.assertEqual(sut.name, "metrics")

    def test_should_return_polars_expressions_for_members(self) -> None:
        """Test that cols() returns polars expressions."""
        # arrange
        sut = ColumnSet(members=["temp_1", "temp_2"], type=float)

        # act
        result = sut.cols()

        # assert
        self.assertEqual(len(result), 2)
        self.assertIn("temp_1", str(result[0]))
        self.assertIn("temp_2", str(result[1]))

    def test_should_return_polars_expressions_for_matched_columns(self) -> None:
        """Test that cols() uses matched_columns when provided."""
        # arrange
        sut = ColumnSet(members=r"temp_\d+", type=float, regex=True)
        matched = ["temp_1", "temp_2", "temp_3"]

        # act
        result = sut.cols(matched_columns=matched)

        # assert
        self.assertEqual(len(result), 3)

    def test_should_raise_for_regex_cols_without_matched(self) -> None:
        """Test that cols() raises for regex patterns without matched_columns."""
        # arrange
        sut = ColumnSet(members=r"temp_\d+", type=float, regex=True)

        # act/assert
        with self.assertRaises(ValueError) as context:
            sut.cols()

        self.assertIn("regex", str(context.exception))

    def test_should_return_polars_expression_for_single_string_member(self) -> None:
        """Test that cols() handles a single string member (non-regex, non-list)."""
        # arrange — construct ColumnSet with string member, regex=False
        # __post_init__ only normalizes when regex=True, so str stays as str
        sut = ColumnSet(members="single_col", type=float, regex=False)

        # act
        result = sut.cols()

        # assert
        self.assertEqual(len(result), 1)
        self.assertIn("single_col", str(result[0]))

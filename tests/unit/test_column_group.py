"""Unit tests for ColumnGroup class."""

import unittest

from typedframes import BaseSchema, Column, ColumnGroup, ColumnSet


class TestColumnGroup(unittest.TestCase):
    """Unit tests for ColumnGroup descriptor."""

    def test_should_set_name_via_descriptor(self) -> None:
        """Test that __set_name__ sets the name attribute."""
        # arrange
        col_a = Column(type=int)
        sut = ColumnGroup(members=[col_a])

        # act
        sut.__set_name__(None, "all_cols")

        # assert
        self.assertEqual(sut.name, "all_cols")

    def test_should_get_column_names_from_columns(self) -> None:
        """Test that get_column_names returns names for Column members."""

        # arrange
        class TestSchema(BaseSchema):
            user_id = Column(type=int)
            email = Column(type=str)
            all_fields = ColumnGroup(members=[user_id, email])

        sut = TestSchema.column_groups()["all_fields"]

        # act
        result = sut.get_column_names()

        # assert
        self.assertEqual(result, ["user_id", "email"])

    def test_should_get_column_names_from_aliased_column(self) -> None:
        """Test that get_column_names uses alias for aliased Column members."""

        # arrange
        class TestSchema(BaseSchema):
            email = Column(type=str, alias="email_address")
            all_fields = ColumnGroup(members=[email])

        sut = TestSchema.column_groups()["all_fields"]

        # act
        result = sut.get_column_names()

        # assert
        self.assertEqual(result, ["email_address"])

    def test_should_get_column_names_from_column_set_with_consumed_map(self) -> None:
        """Test that get_column_names uses consumed_map for ColumnSets."""

        # arrange
        class TestSchema(BaseSchema):
            temps = ColumnSet(members=["temp_1", "temp_2"], type=float)
            all_sensors = ColumnGroup(members=[temps])

        sut = TestSchema.column_groups()["all_sensors"]
        consumed_map = {"temps": ["temp_1", "temp_2"]}

        # act
        result = sut.get_column_names(consumed_map)

        # assert
        self.assertEqual(result, ["temp_1", "temp_2"])

    def test_should_get_column_names_from_column_set_without_consumed_map(self) -> None:
        """Test that get_column_names falls back to members list for ColumnSets."""

        # arrange
        class TestSchema(BaseSchema):
            temps = ColumnSet(members=["temp_1", "temp_2"], type=float)
            all_sensors = ColumnGroup(members=[temps])

        sut = TestSchema.column_groups()["all_sensors"]

        # act
        result = sut.get_column_names()

        # assert
        self.assertEqual(result, ["temp_1", "temp_2"])

    def test_should_get_column_names_from_nested_column_group(self) -> None:
        """Test that get_column_names recurses into nested ColumnGroups."""

        # arrange
        class TestSchema(BaseSchema):
            user_id = Column(type=int)
            email = Column(type=str)
            temps = ColumnSet(members=["temp_1", "temp_2"], type=float)
            user_info = ColumnGroup(members=[user_id, email])
            everything = ColumnGroup(members=[user_info, temps])

        sut = TestSchema.column_groups()["everything"]

        # act
        result = sut.get_column_names()

        # assert
        self.assertEqual(result, ["user_id", "email", "temp_1", "temp_2"])

    def test_should_return_polars_column_expressions(self) -> None:
        """Test that cols() returns polars column expressions."""

        # arrange
        class TestSchema(BaseSchema):
            user_id = Column(type=int)
            email = Column(type=str)
            all_fields = ColumnGroup(members=[user_id, email])

        sut = TestSchema.column_groups()["all_fields"]

        # act
        result = sut.cols()

        # assert
        self.assertEqual(len(result), 2)
        self.assertIn("user_id", str(result[0]))
        self.assertIn("email", str(result[1]))

    def test_should_skip_unknown_member_types(self) -> None:
        """Test that get_column_names silently skips unknown member types."""
        # arrange
        col = Column(type=int)
        col.__set_name__(None, "user_id")
        sut = ColumnGroup(members=[col, "not_a_column_or_set"])  # type: ignore[list-item]
        sut.__set_name__(None, "mixed")

        # act
        result = sut.get_column_names()

        # assert — only the Column is processed, the string is skipped
        self.assertEqual(result, ["user_id"])

    def test_should_get_column_names_from_string_member_column_set(self) -> None:
        """Test that get_column_names handles ColumnSet with string members (non-list)."""
        # arrange — ColumnSet with string member, not regex, no consumed_map
        cs = ColumnSet(members="single_col", type=float, regex=False)
        cs.__set_name__(None, "readings")
        sut = ColumnGroup(members=[cs])
        sut.__set_name__(None, "all_readings")

        # act — consumed_map doesn't have this cs, and members is a string not a list
        result = sut.get_column_names()

        # assert — string member cannot be iterated via extend, so it's skipped
        self.assertEqual(result, [])

    def test_should_return_polars_expressions_with_consumed_map(self) -> None:
        """Test that cols() uses consumed_map for ColumnSets."""

        # arrange
        class TestSchema(BaseSchema):
            temps = ColumnSet(members=["temp_1", "temp_2"], type=float)
            all_sensors = ColumnGroup(members=[temps])

        sut = TestSchema.column_groups()["all_sensors"]
        consumed_map = {"temps": ["temp_1", "temp_2"]}

        # act
        result = sut.cols(consumed_map)

        # assert
        self.assertEqual(len(result), 2)

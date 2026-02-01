"""Unit tests for BaseSchema class."""

import unittest

from typedframes import (
    BaseSchema,
    Column,
    ColumnAliasNotYetDefinedError,
    ColumnGroup,
    ColumnGroupError,
    ColumnSet,
    ColumnSetMembersNotYetDefinedError,
    DefinedLater,
)


class TestBaseSchema(unittest.TestCase):
    """Unit tests for BaseSchema."""

    def test_should_collect_columns(self) -> None:
        """Test that columns() returns all Column definitions."""
        # arrange
        class TestSchema(BaseSchema):
            user_id = Column(type=int)
            email = Column(type=str)

        # act
        result = TestSchema.columns()

        # assert
        self.assertEqual(len(result), 2)
        self.assertIn("user_id", result)
        self.assertIn("email", result)

    def test_should_collect_column_sets(self) -> None:
        """Test that column_sets() returns all ColumnSet definitions."""
        # arrange
        class TestSchema(BaseSchema):
            scores = ColumnSet(members=["score_1", "score_2"], type=float)

        # act
        result = TestSchema.column_sets()

        # assert
        self.assertEqual(len(result), 1)
        self.assertIn("scores", result)

    def test_should_collect_column_groups(self) -> None:
        """Test that column_groups() returns all ColumnGroup definitions."""
        # arrange
        class TestSchema(BaseSchema):
            user_id = Column(type=int)
            email = Column(type=str)
            all_user = ColumnGroup(members=[user_id, email])

        # act
        result = TestSchema.column_groups()

        # assert
        self.assertEqual(len(result), 1)
        self.assertIn("all_user", result)

    def test_should_return_all_column_names(self) -> None:
        """Test that all_column_names() returns column names including aliases."""
        # arrange
        class TestSchema(BaseSchema):
            user_id = Column(type=int)
            email = Column(type=str, alias="email_address")
            scores = ColumnSet(members=["score_1", "score_2"], type=float)

        # act
        result = TestSchema.all_column_names()

        # assert
        self.assertIn("user_id", result)
        self.assertIn("email_address", result)  # Uses alias
        self.assertIn("score_1", result)
        self.assertIn("score_2", result)
        self.assertNotIn("email", result)  # Attribute name not included

    def test_should_compute_column_map(self) -> None:
        """Test that compute_column_map correctly maps DataFrame columns."""
        # arrange
        class TestSchema(BaseSchema):
            user_id = Column(type=int)
            email = Column(type=str)

        df_columns = ["user_id", "email", "extra"]

        # act
        type_map, consumed_map = TestSchema.compute_column_map(df_columns)

        # assert
        self.assertEqual(type_map["user_id"], int)
        self.assertEqual(type_map["email"], str)
        self.assertNotIn("extra", type_map)

    def test_should_match_column_set_members(self) -> None:
        """Test that compute_column_map matches ColumnSet members."""
        # arrange
        class TestSchema(BaseSchema):
            scores = ColumnSet(members=["score_1", "score_2"], type=float)

        df_columns = ["score_1", "score_2", "score_3"]

        # act
        type_map, consumed_map = TestSchema.compute_column_map(df_columns)

        # assert
        self.assertEqual(type_map["score_1"], float)
        self.assertEqual(type_map["score_2"], float)
        self.assertNotIn("score_3", type_map)
        self.assertEqual(consumed_map["scores"], ["score_1", "score_2"])

    def test_should_match_regex_column_set(self) -> None:
        """Test that compute_column_map matches regex ColumnSet patterns."""
        # arrange
        class TestSchema(BaseSchema):
            temps = ColumnSet(members=r"temp_\d+", type=float, regex=True)

        df_columns = ["temp_1", "temp_2", "other"]

        # act
        type_map, consumed_map = TestSchema.compute_column_map(df_columns)

        # assert
        self.assertEqual(type_map["temp_1"], float)
        self.assertEqual(type_map["temp_2"], float)
        self.assertNotIn("other", type_map)

    def test_should_raise_for_defined_later_alias(self) -> None:
        """Test that compute_column_map raises for DefinedLater alias."""
        # arrange
        class TestSchema(BaseSchema):
            user_id = Column(type=int, alias=DefinedLater)

        # act/assert
        with self.assertRaises(ColumnAliasNotYetDefinedError):
            TestSchema.compute_column_map(["user_id"])

    def test_should_raise_for_defined_later_members(self) -> None:
        """Test that compute_column_map raises for DefinedLater members."""
        # arrange
        class TestSchema(BaseSchema):
            scores = ColumnSet(members=DefinedLater, type=float)

        # act/assert
        with self.assertRaises(ColumnSetMembersNotYetDefinedError):
            TestSchema.compute_column_map(["score_1"])

    def test_should_raise_for_column_group_conflict(self) -> None:
        """Test that compute_column_map raises for conflicting ColumnSets."""
        # arrange
        class TestSchema(BaseSchema):
            temps = ColumnSet(members=["temp_1"], type=float)
            readings = ColumnSet(members=["temp_1"], type=float)  # Conflict!

        df_columns = ["temp_1"]

        # act/assert
        with self.assertRaises(ColumnGroupError):
            TestSchema.compute_column_map(df_columns)

    def test_should_allow_greedy_column_sets(self) -> None:
        """Test that greedy=True allows columns to match multiple sets."""
        # arrange
        class TestSchema(BaseSchema):
            greedy_column_sets = True
            temps = ColumnSet(members=["temp_1"], type=float)
            readings = ColumnSet(members=["temp_1"], type=float)

        df_columns = ["temp_1"]

        # act
        type_map, consumed_map = TestSchema.compute_column_map(df_columns)

        # assert
        self.assertEqual(type_map["temp_1"], float)

    def test_should_validate_columns(self) -> None:
        """Test that validate_columns checks for missing columns."""
        # arrange
        class TestSchema(BaseSchema):
            user_id = Column(type=int)
            email = Column(type=str)

        df_columns = ["user_id"]  # Missing email

        # act
        errors = TestSchema.validate_columns(df_columns)

        # assert
        self.assertIn("Missing required column: email", errors)


if __name__ == "__main__":
    unittest.main()

"""Unit tests for BaseSchema class."""

import unittest

import pandas as pd
import polars as pl

from typedframes import (
    BaseSchema,
    Column,
    ColumnGroup,
    ColumnSet,
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

    def test_should_return_cached_column_groups(self) -> None:
        """Test that column_groups() returns cached results on second call."""

        # arrange
        class TestSchema(BaseSchema):
            user_id = Column(type=int)
            email = Column(type=str)
            all_user = ColumnGroup(members=[user_id, email])

        # act
        first = TestSchema.column_groups()
        second = TestSchema.column_groups()

        # assert
        self.assertIs(first, second)

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

    def test_should_return_all_column_names_without_column_sets(self) -> None:
        """Test that all_column_names works when schema has no ColumnSets."""

        # arrange
        class TestSchema(BaseSchema):
            user_id = Column(type=int)
            email = Column(type=str)

        # act
        result = TestSchema.all_column_names()

        # assert
        self.assertEqual(result, ["user_id", "email"])

    def test_should_validate_extra_columns_when_disallowed(self) -> None:
        """Test that validate_columns reports extra columns when allow_extra_columns is False."""

        # arrange
        class StrictSchema(BaseSchema):
            allow_extra_columns = False
            user_id = Column(type=int)

        df_columns = ["user_id", "extra_col"]

        # act
        errors = StrictSchema.validate_columns(df_columns)

        # assert
        self.assertIn("Unexpected column: extra_col", errors)

    def test_should_allow_regex_matched_extra_columns(self) -> None:
        """Test that regex-matched columns are not flagged as extra."""

        # arrange
        class StrictSchema(BaseSchema):
            allow_extra_columns = False
            timestamp = Column(type=str)
            temps = ColumnSet(members=r"temp_\d+", type=float, regex=True)

        df_columns = ["timestamp", "temp_1", "temp_2"]

        # act
        errors = StrictSchema.validate_columns(df_columns)

        # assert
        self.assertEqual(errors, [])

    def test_should_passthrough_from_pandas(self) -> None:
        """Test that from_pandas returns the DataFrame unchanged."""

        # arrange
        class TestSchema(BaseSchema):
            user_id = Column(type=int)

        df = pd.DataFrame({"user_id": [1, 2]})

        # act
        result = TestSchema.from_pandas(df)

        # assert
        self.assertIs(result, df)

    def test_should_passthrough_from_polars(self) -> None:
        """Test that from_polars returns the DataFrame unchanged."""

        # arrange
        class TestSchema(BaseSchema):
            user_id = Column(type=int)

        df = pl.DataFrame({"user_id": [1, 2]})

        # act
        result = TestSchema.from_polars(df)

        # assert
        self.assertIs(result, df)

    def test_should_validate_extra_columns_with_regex_match(self) -> None:
        """Test that regex-matched columns trigger the is_matched=True/break path."""

        # arrange
        class StrictSchema(BaseSchema):
            allow_extra_columns = False
            timestamp = Column(type=str)
            temps = ColumnSet(members=r"temp_\d+", type=float, regex=True)

        # act — "temp_1" matches regex, should NOT be flagged; "extra" should be flagged
        errors = StrictSchema.validate_columns(["timestamp", "temp_1", "extra"])

        # assert
        self.assertEqual(len(errors), 1)
        self.assertIn("Unexpected column: extra", errors)

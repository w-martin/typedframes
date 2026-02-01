"""Unit tests for PandasFrame class."""

import unittest

import pandas as pd

from typedframes import BaseSchema, Column, ColumnSet, PandasFrame


class UserSchema(BaseSchema):
    """Test schema for user data."""

    user_id = Column(type=int)
    email = Column(type=str, alias="email_address")


class SensorSchema(BaseSchema):
    """Test schema with ColumnSet."""

    timestamp = Column(type=str)
    temperatures = ColumnSet(members=["temp_1", "temp_2"], type=float)


class TestPandasFrame(unittest.TestCase):
    """Unit tests for PandasFrame."""

    def test_should_create_from_schema(self) -> None:
        """Test that from_schema creates a typed PandasFrame."""
        # arrange
        raw_df = pd.DataFrame({"user_id": [1, 2], "email_address": ["a@b.com", "c@d.com"]})

        # act
        sut = PandasFrame.from_schema(raw_df, UserSchema)

        # assert
        self.assertIsInstance(sut, PandasFrame)
        self.assertEqual(sut.schema, UserSchema)

    def test_should_access_column_by_attribute(self) -> None:
        """Test that columns can be accessed by schema attribute name."""
        # arrange
        raw_df = pd.DataFrame({"user_id": [1, 2], "email_address": ["a@b.com", "c@d.com"]})
        sut = PandasFrame.from_schema(raw_df, UserSchema)

        # act
        result = sut.user_id

        # assert
        self.assertIsInstance(result, pd.Series)
        self.assertEqual(result.tolist(), [1, 2])

    def test_should_access_aliased_column_by_attribute(self) -> None:
        """Test that aliased columns can be accessed by schema attribute name."""
        # arrange
        raw_df = pd.DataFrame({"user_id": [1, 2], "email_address": ["a@b.com", "c@d.com"]})
        sut = PandasFrame.from_schema(raw_df, UserSchema)

        # act
        result = sut.email  # Attribute name, maps to email_address

        # assert
        self.assertIsInstance(result, pd.Series)
        self.assertEqual(result.tolist(), ["a@b.com", "c@d.com"])

    def test_should_access_column_set_by_attribute(self) -> None:
        """Test that ColumnSet can be accessed by attribute name."""
        # arrange
        raw_df = pd.DataFrame({"timestamp": ["t1"], "temp_1": [20.0], "temp_2": [25.0]})
        sut = PandasFrame.from_schema(raw_df, SensorSchema)

        # act
        result = sut.temperatures

        # assert
        self.assertIsInstance(result, pd.DataFrame)
        self.assertEqual(list(result.columns), ["temp_1", "temp_2"])

    def test_should_preserve_type_after_filtering(self) -> None:
        """Test that filtering preserves PandasFrame type."""
        # arrange
        raw_df = pd.DataFrame({"user_id": [1, 2], "email_address": ["a@b.com", "c@d.com"]})
        sut = PandasFrame.from_schema(raw_df, UserSchema)

        # act
        result = sut[sut.user_id > 1]

        # assert
        self.assertIsInstance(result, PandasFrame)
        self.assertEqual(result.schema, UserSchema)

    def test_should_preserve_type_after_selection(self) -> None:
        """Test that column selection preserves PandasFrame type."""
        # arrange
        raw_df = pd.DataFrame({"user_id": [1, 2], "email_address": ["a@b.com", "c@d.com"]})
        sut = PandasFrame.from_schema(raw_df, UserSchema)

        # act
        result = sut[["user_id"]]

        # assert
        self.assertIsInstance(result, PandasFrame)

    def test_should_convert_to_plain_pandas(self) -> None:
        """Test that to_pandas returns a plain DataFrame."""
        # arrange
        raw_df = pd.DataFrame({"user_id": [1, 2], "email_address": ["a@b.com", "c@d.com"]})
        sut = PandasFrame.from_schema(raw_df, UserSchema)

        # act
        result = sut.to_pandas()

        # assert
        self.assertIsInstance(result, pd.DataFrame)
        self.assertNotIsInstance(result, PandasFrame)

    def test_should_fallback_to_pandas_methods(self) -> None:
        """Test that standard pandas methods work."""
        # arrange
        raw_df = pd.DataFrame({"user_id": [1, 2], "email_address": ["a@b.com", "c@d.com"]})
        sut = PandasFrame.from_schema(raw_df, UserSchema)

        # act
        shape = sut.shape
        columns = list(sut.columns)

        # assert
        self.assertEqual(shape, (2, 2))
        self.assertEqual(columns, ["user_id", "email_address"])


if __name__ == "__main__":
    unittest.main()

"""Unit tests for PandasFrame class."""

import unittest

import pandas as pd

from typedframes import BaseSchema, Column, ColumnGroup, ColumnSet, PandasFrame


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

    def test_should_preserve_type_after_merge(self) -> None:
        """Test that merge preserves PandasFrame type and schema."""
        # arrange
        df1 = pd.DataFrame({"user_id": [1, 2], "email_address": ["a@b.com", "c@d.com"]})
        df2 = pd.DataFrame({"user_id": [1, 2], "name": ["Alice", "Bob"]})
        sut = PandasFrame.from_schema(df1, UserSchema)

        # act
        result = sut.merge(df2, on="user_id")

        # assert
        self.assertIsInstance(result, PandasFrame)
        self.assertEqual(result.schema, UserSchema)
        self.assertEqual(len(result), 2)

    def test_should_preserve_type_after_join(self) -> None:
        """Test that df.join() preserves PandasFrame type and schema."""
        # arrange
        df1 = pd.DataFrame({"user_id": [1, 2], "email_address": ["a@b.com", "c@d.com"]})
        df2 = pd.DataFrame({"name": ["Alice", "Bob"]}, index=[0, 1])
        sut = PandasFrame.from_schema(df1, UserSchema)

        # act
        result = sut.join(df2)

        # assert
        self.assertIsInstance(result, PandasFrame)
        self.assertEqual(result.schema, UserSchema)

    def test_should_access_columns_after_merge(self) -> None:
        """Test that schema column access works after merge."""
        # arrange
        df1 = pd.DataFrame({"user_id": [1, 2], "email_address": ["a@b.com", "c@d.com"]})
        df2 = pd.DataFrame({"user_id": [1, 2], "name": ["Alice", "Bob"]})
        sut = PandasFrame.from_schema(df1, UserSchema)
        merged = sut.merge(df2, on="user_id")

        # act
        user_ids = merged.user_id
        emails = merged.email

        # assert
        self.assertEqual(user_ids.tolist(), [1, 2])
        self.assertEqual(emails.tolist(), ["a@b.com", "c@d.com"])

    def test_should_access_columns_after_filter(self) -> None:
        """Test that schema column access works after filtering."""
        # arrange
        raw_df = pd.DataFrame({"user_id": [1, 2, 3], "email_address": ["a@b.com", "c@d.com", "e@f.com"]})
        sut = PandasFrame.from_schema(raw_df, UserSchema)
        filtered = sut[sut.user_id > 1]

        # act
        user_ids = filtered.user_id
        emails = filtered.email

        # assert
        self.assertEqual(user_ids.tolist(), [2, 3])
        self.assertEqual(emails.tolist(), ["c@d.com", "e@f.com"])

    def test_should_access_columns_after_select(self) -> None:
        """Test that schema column access works after column selection."""
        # arrange
        raw_df = pd.DataFrame({"user_id": [1, 2], "email_address": ["a@b.com", "c@d.com"]})
        sut = PandasFrame.from_schema(raw_df, UserSchema)
        selected = sut[["user_id", "email_address"]]

        # act
        user_ids = selected.user_id

        # assert
        self.assertEqual(user_ids.tolist(), [1, 2])

    def test_should_access_column_group_by_attribute(self) -> None:
        """Test that ColumnGroups can be accessed by attribute name."""

        # arrange
        class GroupedSchema(BaseSchema):
            user_id = Column(type=int)
            email = Column(type=str)
            all_fields = ColumnGroup(members=[user_id, email])

        raw_df = pd.DataFrame({"user_id": [1, 2], "email": ["a@b.com", "c@d.com"]})
        sut = PandasFrame.from_schema(raw_df, GroupedSchema)

        # act
        result = sut.all_fields

        # assert
        self.assertIsInstance(result, pd.DataFrame)
        self.assertEqual(list(result.columns), ["user_id", "email"])

    def test_should_fallback_to_pandas_when_no_schema(self) -> None:
        """Test that PandasFrame without schema falls back to pandas attribute access."""
        # arrange
        sut = PandasFrame({"user_id": [1, 2], "email": ["a@b.com", "c@d.com"]})

        # act
        result = sut.shape

        # assert
        self.assertEqual(result, (2, 2))

    def test_should_fallback_for_non_schema_attribute(self) -> None:
        """Test that non-schema attributes fall through to pandas."""
        # arrange
        raw_df = pd.DataFrame({"user_id": [1, 2], "email_address": ["a@b.com", "c@d.com"]})
        sut = PandasFrame.from_schema(raw_df, UserSchema)

        # act — dtypes is a pandas attribute, not in schema
        result = sut.dtypes

        # assert
        self.assertIsNotNone(result)

    def test_should_create_with_explicit_consumed_map(self) -> None:
        """Test that from_schema accepts a pre-computed consumed_map."""
        # arrange
        raw_df = pd.DataFrame({"timestamp": ["t1"], "temp_1": [20.0], "temp_2": [25.0]})
        consumed_map = {"temperatures": ["temp_1", "temp_2"]}

        # act
        sut = PandasFrame.from_schema(raw_df, SensorSchema, column_consumed_map=consumed_map)

        # assert
        self.assertIsInstance(sut, PandasFrame)
        self.assertEqual(sut._column_consumed_map, consumed_map)

    def test_should_handle_getattr_for_underscore_attrs(self) -> None:
        """Test that __getattr__ delegates underscore attrs to object.__getattribute__."""
        # arrange
        raw_df = pd.DataFrame({"user_id": [1, 2], "email_address": ["a@b.com", "c@d.com"]})
        sut = PandasFrame.from_schema(raw_df, UserSchema)

        # act/assert — access a non-existent underscore attr via __getattr__
        # Since __getattr__ is only called when __getattribute__ fails,
        # we need to access a private attr that doesn't exist
        with self.assertRaises(AttributeError):
            sut.__getattr__("_nonexistent_private_attr")

    def test_should_handle_getattr_without_schema_set(self) -> None:
        """Test that __getattr__ handles missing _schema_class gracefully."""
        # arrange — create a PandasFrame then delete the schema attr to simulate
        # the internal state during pandas construction
        sut = PandasFrame({"user_id": [1, 2]})
        del sut._schema_class

        # act — accessing a non-existent attr should hit the AttributeError
        # fallback path (lines 125-127) and then super().__getattribute__
        with self.assertRaises(AttributeError):
            _ = sut.nonexistent_attr

    def test_should_fallback_to_super_for_unknown_attr(self) -> None:
        """Test that unknown attributes fall through to super().__getattribute__."""
        # arrange
        raw_df = pd.DataFrame({"user_id": [1, 2], "email_address": ["a@b.com", "c@d.com"]})
        sut = PandasFrame.from_schema(raw_df, UserSchema)

        # act/assert — access a non-existent attr should raise AttributeError
        with self.assertRaises(AttributeError):
            _ = sut.nonexistent_column

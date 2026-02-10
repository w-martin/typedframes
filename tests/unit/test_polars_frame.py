"""Unit tests for Polars integration with typedframes."""

import unittest
from typing import Annotated

import polars as pl

from typedframes import BaseSchema, Column, ColumnSet
from typedframes.polars import PolarsFrame


class UserSchema(BaseSchema):
    """Test schema for user data."""

    user_id = Column(type=int)
    email = Column(type=str, alias="email_address")


class OrderSchema(BaseSchema):
    """Test schema for order data."""

    order_id = Column(type=int)
    user_id = Column(type=int)
    amount = Column(type=float)


class SensorSchema(BaseSchema):
    """Test schema with ColumnSet."""

    timestamp = Column(type=str)
    temperatures = ColumnSet(members=["temp_1", "temp_2"], type=float)


class TestPolarsFrame(unittest.TestCase):
    """Unit tests for Polars integration."""

    def test_annotated_pattern_preserves_through_filter(self) -> None:
        """Test that Annotated type pattern works with filter operations."""
        # arrange
        df: Annotated[pl.DataFrame, UserSchema] = pl.DataFrame(
            {
                "user_id": [1, 2, 3],
                "email_address": ["a@b.com", "c@d.com", "e@f.com"],
            }
        )

        # act
        result: Annotated[pl.DataFrame, UserSchema] = df.filter(pl.col("user_id") > 1)

        # assert
        self.assertIsInstance(result, pl.DataFrame)
        self.assertEqual(len(result), 2)
        self.assertEqual(result["user_id"].to_list(), [2, 3])

    def test_annotated_pattern_preserves_through_select(self) -> None:
        """Test that Annotated type pattern works with select operations."""
        # arrange
        df: Annotated[pl.DataFrame, UserSchema] = pl.DataFrame(
            {
                "user_id": [1, 2],
                "email_address": ["a@b.com", "c@d.com"],
            }
        )

        # act
        result = df.select(pl.col("user_id"), pl.col("email_address"))

        # assert
        self.assertIsInstance(result, pl.DataFrame)
        self.assertEqual(list(result.columns), ["user_id", "email_address"])

    def test_schema_column_access_in_filter(self) -> None:
        """Test that Schema.col works in filter expressions."""
        # arrange
        df: PolarsFrame[UserSchema] = pl.DataFrame(
            {
                "user_id": [1, 2, 3],
                "email_address": ["a@b.com", "c@d.com", "e@f.com"],
            }
        )

        # act
        result = df.filter(UserSchema.user_id.col > 1)

        # assert
        self.assertEqual(len(result), 2)
        self.assertEqual(result["user_id"].to_list(), [2, 3])

    def test_schema_column_access_in_select(self) -> None:
        """Test that Schema.col works in select expressions."""
        # arrange
        df: PolarsFrame[UserSchema] = pl.DataFrame(
            {
                "user_id": [1, 2],
                "email_address": ["a@b.com", "c@d.com"],
            }
        )

        # act
        result = df.select(UserSchema.user_id.col, UserSchema.email.col)

        # assert
        self.assertEqual(list(result.columns), ["user_id", "email_address"])

    def test_schema_column_access_in_join(self) -> None:
        """Test that Schema.col works in join expressions."""
        # arrange
        users: PolarsFrame[UserSchema] = pl.DataFrame(
            {
                "user_id": [1, 2],
                "email_address": ["a@b.com", "c@d.com"],
            }
        )
        orders: PolarsFrame[OrderSchema] = pl.DataFrame(
            {
                "order_id": [100, 101],
                "user_id": [1, 2],
                "amount": [50.0, 75.0],
            }
        )

        # act
        result = users.join(orders, on=str(UserSchema.user_id))

        # assert
        self.assertEqual(len(result), 2)
        self.assertIn("order_id", result.columns)
        self.assertIn("amount", result.columns)

    def test_schema_column_access_with_alias(self) -> None:
        """Test that aliased columns work correctly with polars expressions."""
        # arrange
        df: PolarsFrame[UserSchema] = pl.DataFrame(
            {
                "user_id": [1, 2],
                "email_address": ["a@b.com", "c@d.com"],
            }
        )

        # act - email.col should use the alias "email_address"
        result = df.select(UserSchema.email.col)

        # assert
        self.assertEqual(list(result.columns), ["email_address"])
        self.assertEqual(result["email_address"].to_list(), ["a@b.com", "c@d.com"])

    def test_column_set_cols_in_select(self) -> None:
        """Test that ColumnSet.cols() works in select expressions."""
        # arrange
        df: PolarsFrame[SensorSchema] = pl.DataFrame(
            {
                "timestamp": ["t1", "t2"],
                "temp_1": [20.0, 21.0],
                "temp_2": [25.0, 26.0],
            }
        )

        # act
        result = df.select(SensorSchema.temperatures.cols())

        # assert
        self.assertEqual(list(result.columns), ["temp_1", "temp_2"])

    def test_should_return_annotated_type_from_class_getitem(self) -> None:
        """Test that PolarsFrame[Schema] returns an Annotated type."""
        # act
        result = PolarsFrame[UserSchema]

        # assert
        self.assertIsNotNone(result)

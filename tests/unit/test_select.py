"""Unit tests for BaseSchema.select() classmethod."""

import unittest

from typedframes import (
    BaseSchema,
    Column,
    ColumnSet,
)


class Users(BaseSchema):
    """Test schema for users."""

    user_id = Column(type=int)
    email = Column(type=str)
    name = Column(type=str)


class Orders(BaseSchema):
    """Test schema for orders."""

    order_id = Column(type=int)
    user_id = Column(type=int)
    total = Column(type=float)


class SensorData(BaseSchema):
    """Test schema with ColumnSet."""

    timestamp = Column(type=str)
    sensors = ColumnSet(members=["temp_1", "temp_2"], type=float)


class MultiSensorData(BaseSchema):
    """Test schema with multiple ColumnSets."""

    timestamp = Column(type=str)
    temps = ColumnSet(members=["temp_1", "temp_2"], type=float)
    pressures = ColumnSet(members=["pres_1", "pres_2"], type=float)


class TestSelect(unittest.TestCase):
    """Tests for BaseSchema.select() classmethod."""

    def test_should_select_specified_columns(self) -> None:
        """Test that select creates schema with only specified columns."""
        # act
        selected = Users.select([Users.user_id, Users.email])

        # assert
        columns = selected.columns()
        self.assertEqual(len(columns), 2)
        self.assertIn("user_id", columns)
        self.assertIn("email", columns)
        self.assertNotIn("name", columns)

    def test_should_preserve_column_types(self) -> None:
        """Test that selected columns preserve their types."""
        # act
        selected = Users.select([Users.user_id, Users.email])

        # assert
        columns = selected.columns()
        self.assertEqual(columns["user_id"].type, int)
        self.assertEqual(columns["email"].type, str)

    def test_should_use_custom_name(self) -> None:
        """Test that custom schema name is used."""
        # act
        selected = Users.select([Users.user_id], name="UserIds")

        # assert
        self.assertEqual(selected.__name__, "UserIds")

    def test_should_use_default_name(self) -> None:
        """Test that default schema name is generated."""
        # act
        selected = Users.select([Users.user_id])

        # assert
        self.assertEqual(selected.__name__, "Users_Select")

    def test_should_select_column_sets(self) -> None:
        """Test that ColumnSets can be selected."""
        # act
        selected = SensorData.select([SensorData.timestamp, SensorData.sensors])

        # assert
        self.assertIn("timestamp", selected.columns())
        self.assertIn("sensors", selected.column_sets())

    def test_should_reject_cross_schema_column(self) -> None:
        """Test that passing a column from another schema raises ValueError."""
        # act/assert
        with self.assertRaises(ValueError) as ctx:
            Users.select([Orders.order_id])

        self.assertIn("does not belong to Users", str(ctx.exception))

    def test_should_reject_cross_schema_column_set(self) -> None:
        """Test that passing a ColumnSet from another schema raises ValueError."""
        # act/assert
        with self.assertRaises(ValueError) as ctx:
            Users.select([SensorData.sensors])

        self.assertIn("does not belong to Users", str(ctx.exception))

    def test_should_reject_foreign_column_set_when_schema_has_column_sets(self) -> None:
        """Test that a foreign ColumnSet is rejected even when schema has its own ColumnSets."""
        # act/assert — SensorData has column sets, but Orders.order_id is a Column not ColumnSet
        # Use MultiSensorData which has column sets, and pass a foreign ColumnSet
        with self.assertRaises(ValueError) as ctx:
            MultiSensorData.select([SensorData.sensors])

        self.assertIn("does not belong to MultiSensorData", str(ctx.exception))

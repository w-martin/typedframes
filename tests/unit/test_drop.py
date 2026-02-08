"""Unit tests for BaseSchema.drop() classmethod."""

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


class UserWithPassword(BaseSchema):
    """Test schema with sensitive column."""

    user_id = Column(type=int)
    email = Column(type=str)
    password_hash = Column(type=str)


class MultiSensorData(BaseSchema):
    """Test schema with multiple ColumnSets."""

    timestamp = Column(type=str)
    temps = ColumnSet(members=["temp_1", "temp_2"], type=float)
    pressures = ColumnSet(members=["pres_1", "pres_2"], type=float)


class TestDrop(unittest.TestCase):
    """Tests for BaseSchema.drop() classmethod."""

    def test_should_drop_specified_columns(self) -> None:
        """Test that drop excludes specified columns."""
        # act
        dropped = Users.drop([Users.name])

        # assert
        columns = dropped.columns()
        self.assertEqual(len(columns), 2)
        self.assertIn("user_id", columns)
        self.assertIn("email", columns)
        self.assertNotIn("name", columns)

    def test_should_preserve_column_types(self) -> None:
        """Test that remaining columns preserve their types."""
        # act
        dropped = Users.drop([Users.name])

        # assert
        columns = dropped.columns()
        self.assertEqual(columns["user_id"].type, int)
        self.assertEqual(columns["email"].type, str)

    def test_should_use_custom_name(self) -> None:
        """Test that custom schema name is used."""
        # act
        dropped = Users.drop([Users.name], name="UserNoName")

        # assert
        self.assertEqual(dropped.__name__, "UserNoName")

    def test_should_use_default_name(self) -> None:
        """Test that default schema name is generated."""
        # act
        dropped = Users.drop([Users.name])

        # assert
        self.assertEqual(dropped.__name__, "Users_Drop")

    def test_should_drop_column_sets(self) -> None:
        """Test that ColumnSets can be dropped."""
        # act
        dropped = SensorData.drop([SensorData.sensors])

        # assert
        self.assertIn("timestamp", dropped.columns())
        self.assertNotIn("sensors", dropped.column_sets())

    def test_should_keep_other_column_sets_when_dropping_one(self) -> None:
        """Test that dropping one ColumnSet preserves others."""
        # act
        dropped = MultiSensorData.drop([MultiSensorData.temps])

        # assert
        self.assertNotIn("temps", dropped.column_sets())
        self.assertIn("pressures", dropped.column_sets())
        self.assertIn("timestamp", dropped.columns())

    def test_should_reject_cross_schema_column(self) -> None:
        """Test that passing a column from another schema raises ValueError."""
        # act/assert
        with self.assertRaises(ValueError) as ctx:
            Users.drop([Orders.order_id])

        self.assertIn("does not belong to Users", str(ctx.exception))

    def test_should_drop_sensitive_columns(self) -> None:
        """Test dropping sensitive columns from a schema."""
        # act
        public = UserWithPassword.drop([UserWithPassword.password_hash])

        # assert
        columns = public.columns()
        self.assertEqual(len(columns), 2)
        self.assertIn("user_id", columns)
        self.assertIn("email", columns)
        self.assertNotIn("password_hash", columns)

"""Unit tests for combine_schemas function."""

import unittest

from typedframes import (
    BaseSchema,
    Column,
    ColumnSet,
    SchemaConflictError,
    combine_schemas,
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


class OrdersConflict(BaseSchema):
    """Test schema with conflicting user_id type."""

    order_id = Column(type=int)
    user_id = Column(type=str)  # Conflicts with Users.user_id (int)
    total = Column(type=float)


class SensorData(BaseSchema):
    """Test schema with ColumnSet."""

    timestamp = Column(type=str)
    sensors = ColumnSet(members=["temp_1", "temp_2"], type=float)


class TestCombineSchemas(unittest.TestCase):
    """Tests for combine_schemas function."""

    def test_should_combine_two_schemas(self) -> None:
        """Test that combine_schemas creates a schema with all columns."""
        # act
        combined = combine_schemas(Users, Orders)

        # assert
        columns = combined.columns()
        self.assertIn("user_id", columns)
        self.assertIn("email", columns)
        self.assertIn("name", columns)
        self.assertIn("order_id", columns)
        self.assertIn("total", columns)

    def test_should_preserve_column_types(self) -> None:
        """Test that combined schema preserves column types."""
        # act
        combined = combine_schemas(Users, Orders)

        # assert
        columns = combined.columns()
        self.assertEqual(columns["user_id"].type, int)
        self.assertEqual(columns["email"].type, str)
        self.assertEqual(columns["total"].type, float)

    def test_should_use_custom_name(self) -> None:
        """Test that custom schema name is used."""
        # act
        combined = combine_schemas(Users, Orders, name="UserOrders")

        # assert
        self.assertEqual(combined.__name__, "UserOrders")

    def test_should_use_default_name(self) -> None:
        """Test that default schema name is generated."""
        # act
        combined = combine_schemas(Users, Orders)

        # assert
        self.assertEqual(combined.__name__, "Users_Orders")

    def test_should_combine_with_column_sets(self) -> None:
        """Test that ColumnSets are included in combined schema."""
        # act
        combined = combine_schemas(Users, SensorData)

        # assert
        self.assertIn("sensors", combined.column_sets())

    def test_should_allow_identical_overlap(self) -> None:
        """Test that overlapping columns with identical types are allowed."""
        # act
        combined = combine_schemas(Users, Orders)

        # assert — user_id appears in both with type int, should succeed
        columns = combined.columns()
        self.assertEqual(columns["user_id"].type, int)
        self.assertEqual(len(columns), 5)

    def test_should_raise_on_conflicting_types(self) -> None:
        """Test that overlapping columns with different types raise SchemaConflictError."""
        # act/assert
        with self.assertRaises(SchemaConflictError) as ctx:
            combine_schemas(Users, OrdersConflict)

        self.assertIn("user_id", str(ctx.exception))
        self.assertIn("int", str(ctx.exception))
        self.assertIn("str", str(ctx.exception))

    def test_should_combine_schemas_with_string_member_column_set(self) -> None:
        """Test combining schemas where ColumnSet has a string member (non-regex)."""

        # arrange
        class SchemaA(BaseSchema):
            """Schema with string-member ColumnSet."""

            ts = Column(type=str)
            readings = ColumnSet(members="single_col", type=float, regex=False)

        class SchemaB(BaseSchema):
            """Schema with a column."""

            value = Column(type=int)

        # act
        combined = SchemaA + SchemaB

        # assert
        self.assertIn("ts", combined.columns())
        self.assertIn("value", combined.columns())
        self.assertIn("readings", combined.column_sets())

    def test_should_deduplicate_column_sets_by_name(self) -> None:
        """Test that duplicate ColumnSet names from schema_b are skipped."""

        # arrange
        class SchemaA(BaseSchema):
            """Schema with sensors ColumnSet."""

            sensors = ColumnSet(members=["temp_1"], type=float)

        class SchemaB(BaseSchema):
            """Schema with same-named ColumnSet."""

            sensors = ColumnSet(members=["temp_2"], type=float)

        # act
        combined = SchemaA + SchemaB

        # assert — schema_a's sensors should win
        cs = combined.column_sets()["sensors"]
        self.assertEqual(cs.members, ["temp_1"])

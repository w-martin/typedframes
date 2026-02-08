"""Unit tests for the + operator on schemas."""

import unittest

from typedframes import (
    BaseSchema,
    Column,
    SchemaConflictError,
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


class TestSchemaAddOperator(unittest.TestCase):
    """Tests for the + operator on schemas."""

    def test_should_combine_with_plus_operator(self) -> None:
        """Test that + operator combines schemas."""
        # act
        combined = Users + Orders

        # assert
        columns = combined.columns()
        self.assertIn("user_id", columns)
        self.assertIn("email", columns)
        self.assertIn("order_id", columns)

    def test_should_be_commutative(self) -> None:
        """Test that A + B has same columns as B + A."""
        # act
        combined_ab = Users + Orders
        combined_ba = Orders + Users

        # assert
        self.assertEqual(
            set(combined_ab.columns().keys()),
            set(combined_ba.columns().keys()),
        )

    def test_should_raise_on_conflicting_types_via_operator(self) -> None:
        """Test that + operator raises SchemaConflictError for type conflicts."""
        # act/assert
        with self.assertRaises(SchemaConflictError):
            Users + OrdersConflict

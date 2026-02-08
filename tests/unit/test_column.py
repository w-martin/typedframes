"""Unit tests for Column class."""

import unittest
from typing import Any

from typedframes import Column


class TestColumn(unittest.TestCase):
    """Unit tests for Column descriptor."""

    def test_should_create_column_with_defaults(self) -> None:
        """Test that Column can be created with default values."""
        # arrange/act
        sut = Column()

        # assert
        self.assertIs(sut.type, Any)
        self.assertIsNone(sut.alias)
        self.assertFalse(sut.nullable)
        self.assertEqual(sut.description, "")

    def test_should_create_column_with_type(self) -> None:
        """Test that Column can be created with a specific type."""
        # arrange/act
        sut = Column(type=int)

        # assert
        self.assertEqual(sut.type, int)

    def test_should_create_column_with_alias(self) -> None:
        """Test that Column can be created with an alias."""
        # arrange/act
        sut = Column(type=str, alias="user_email")

        # assert
        self.assertEqual(sut.alias, "user_email")

    def test_should_set_name_via_descriptor(self) -> None:
        """Test that __set_name__ sets the name attribute."""
        # arrange
        sut = Column(type=int)

        # act
        sut.__set_name__(None, "user_id")

        # assert
        self.assertEqual(sut.name, "user_id")

    def test_should_return_alias_as_column_name_when_set(self) -> None:
        """Test that column_name returns alias when set."""
        # arrange
        sut = Column(type=str, alias="email_address")
        sut.__set_name__(None, "email")

        # act
        result = sut.column_name

        # assert
        self.assertEqual(result, "email_address")

    def test_should_return_name_as_column_name_when_no_alias(self) -> None:
        """Test that column_name returns name when no alias."""
        # arrange
        sut = Column(type=str)
        sut.__set_name__(None, "email")

        # act
        result = sut.column_name

        # assert
        self.assertEqual(result, "email")

    def test_should_return_polars_expression(self) -> None:
        """Test that col property returns polars expression."""
        # arrange
        sut = Column(type=int)
        sut.__set_name__(None, "user_id")

        # act
        result = sut.col

        # assert
        # Just verify it's a polars expression (can't easily compare expressions)
        self.assertIn("user_id", str(result))

    def test_should_return_alias_in_polars_expression(self) -> None:
        """Test that col property uses alias when set."""
        # arrange
        sut = Column(type=str, alias="email_address")
        sut.__set_name__(None, "email")

        # act
        result = sut.col

        # assert
        self.assertIn("email_address", str(result))

    def test_should_return_column_name_as_string(self) -> None:
        """Test that __str__ returns the effective column name."""
        # arrange
        sut = Column(type=str, alias="email_address")
        sut.__set_name__(None, "email")

        # act
        result = str(sut)

        # assert
        self.assertEqual(result, "email_address")

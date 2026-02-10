"""Unit tests for MissingDependencyError."""

import builtins
import unittest
from unittest.mock import patch

from typedframes import Column, ColumnGroup, ColumnSet, MissingDependencyError


class TestMissingDependencyError(unittest.TestCase):
    """Unit tests for MissingDependencyError."""

    def test_should_inherit_from_import_error(self) -> None:
        """Test that MissingDependencyError is an ImportError subclass."""
        # arrange
        err = MissingDependencyError("polars", "Column.col")

        # assert
        self.assertIsInstance(err, ImportError)

    def test_should_store_package_attribute(self) -> None:
        """Test that package attribute is stored correctly."""
        # arrange/act
        err = MissingDependencyError("polars", "Column.col")

        # assert
        self.assertEqual(err.package, "polars")

    def test_should_store_feature_attribute(self) -> None:
        """Test that feature attribute is stored correctly."""
        # arrange/act
        err = MissingDependencyError("polars", "Column.col")

        # assert
        self.assertEqual(err.feature, "Column.col")

    def test_should_format_message_with_install_hint(self) -> None:
        """Test that message includes install instructions."""
        # arrange/act
        err = MissingDependencyError("polars", "Column.col")

        # assert
        self.assertIn("polars is required for Column.col", str(err))
        self.assertIn("pip install typedframes[polars]", str(err))

    def test_should_raise_from_column_col_when_polars_missing(self) -> None:
        """Test that Column.col raises MissingDependencyError when polars is not installed."""
        # arrange
        col = Column(type=int)
        col.__set_name__(None, "test_col")

        original_import = builtins.__import__

        def mock_import(name: str, *args: object, **kwargs: object) -> object:
            if name == "polars":
                raise ImportError
            return original_import(name, *args, **kwargs)

        # act/assert
        with (
            patch("builtins.__import__", side_effect=mock_import),
            self.assertRaises(MissingDependencyError) as ctx,
        ):
            _ = col.col

        self.assertEqual(ctx.exception.package, "polars")
        self.assertEqual(ctx.exception.feature, "Column.col")

    def test_should_raise_from_column_set_cols_when_polars_missing(self) -> None:
        """Test that ColumnSet.cols raises MissingDependencyError when polars is not installed."""
        # arrange
        cs = ColumnSet(members=["a", "b"], type=float)
        cs.__set_name__(None, "test_cs")

        original_import = builtins.__import__

        def mock_import(name: str, *args: object, **kwargs: object) -> object:
            if name == "polars":
                raise ImportError
            return original_import(name, *args, **kwargs)

        # act/assert
        with (
            patch("builtins.__import__", side_effect=mock_import),
            self.assertRaises(MissingDependencyError) as ctx,
        ):
            cs.cols()

        self.assertEqual(ctx.exception.package, "polars")
        self.assertEqual(ctx.exception.feature, "ColumnSet.cols")

    def test_should_raise_from_column_group_cols_when_polars_missing(self) -> None:
        """Test that ColumnGroup.cols raises MissingDependencyError when polars is not installed."""
        # arrange
        col = Column(type=int)
        col.__set_name__(None, "test_col")
        cg = ColumnGroup(members=[col])
        cg.__set_name__(None, "test_group")

        original_import = builtins.__import__

        def mock_import(name: str, *args: object, **kwargs: object) -> object:
            if name == "polars":
                raise ImportError
            return original_import(name, *args, **kwargs)

        # act/assert
        with (
            patch("builtins.__import__", side_effect=mock_import),
            self.assertRaises(MissingDependencyError) as ctx,
        ):
            cg.cols()

        self.assertEqual(ctx.exception.package, "polars")
        self.assertEqual(ctx.exception.feature, "ColumnGroup.cols")

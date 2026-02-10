"""Unit tests for the mypy plugin."""

import json
import sys
import unittest
from pathlib import Path
from unittest.mock import MagicMock, patch

from mypy.options import Options
from typedframes_lint.mypy import LinterNotFoundError, plugin
from typedframes_lint.mypy import TypedFramesPlugin as PandasLinterPlugin


class TestPandasLinterPluginUnit(unittest.TestCase):
    """Unit tests for the PandasLinterPlugin class."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        # arrange
        self.plugin = PandasLinterPlugin(Options())
        self.test_file = "test.py"
        self.error_data = [{"line": 10, "message": "Column 'foo' does not exist"}]

    def test_should_report_error_on_exact_line_match(self) -> None:
        """Test that errors are reported on exact line matches."""
        # arrange - mock the rust extension
        mock_check_file = MagicMock(return_value=json.dumps(self.error_data))

        with (
            patch("typedframes_lint.mypy.get_project_root") as mock_root,
            patch("typedframes_lint.mypy.is_enabled") as mock_enabled,
            patch.dict(sys.modules, {"typedframes_lint._rust_linter": MagicMock(check_file=mock_check_file)}),
        ):
            mock_enabled.return_value = True
            mock_root.return_value = Path()

            context = MagicMock()
            context.api.path = self.test_file
            context.context.line = 10

            # act
            self.plugin.check_column_access(context)

            # assert
            context.api.fail.assert_called_once_with(
                self.error_data[0]["message"],
                context.context,
            )

        # Test non-match branch
        new_plugin = PandasLinterPlugin(Options())  # New plugin to avoid cache
        mock_check_file_empty = MagicMock(return_value=json.dumps([]))

        with (
            patch("typedframes_lint.mypy.get_project_root") as mock_root,
            patch("typedframes_lint.mypy.is_enabled") as mock_enabled,
            patch.dict(sys.modules, {"typedframes_lint._rust_linter": MagicMock(check_file=mock_check_file_empty)}),
        ):
            mock_enabled.return_value = True
            mock_root.return_value = Path()

            context = MagicMock()
            context.api.path = "other.py"  # Different file to be sure
            context.context.line = 10
            context.api.fail = MagicMock()

            # act
            new_plugin.check_column_access(context)

            # assert
            context.api.fail.assert_not_called()

    def test_should_report_error_on_fuzzy_line_match(self) -> None:
        """Test that errors are reported on fuzzy line matches within tolerance."""
        # arrange - mock the rust extension
        mock_check_file = MagicMock(return_value=json.dumps(self.error_data))

        with (
            patch("typedframes_lint.mypy.get_project_root") as mock_root,
            patch("typedframes_lint.mypy.is_enabled") as mock_enabled,
            patch.dict(sys.modules, {"typedframes_lint._rust_linter": MagicMock(check_file=mock_check_file)}),
        ):
            mock_enabled.return_value = True
            mock_root.return_value = Path()

            context = MagicMock()
            context.api.path = self.test_file
            context.context.line = 11  # One line off

            # act
            self.plugin.check_column_access(context)

            # assert
            context.api.fail.assert_called_once_with(
                self.error_data[0]["message"],
                context.context,
            )

    def test_should_return_default_when_no_path(self) -> None:
        """Test that default return type is used when no path is available."""
        # arrange
        context = MagicMock()
        context.api.path = None
        context.default_return_type = MagicMock()

        # act
        result = self.plugin.check_column_access(context)

        # assert
        self.assertEqual(result, context.default_return_type)

    def test_should_not_run_when_disabled(self) -> None:
        """Test that linter does not run when disabled in config."""
        # arrange
        with patch("typedframes_lint.mypy.is_enabled") as mock_enabled:
            mock_enabled.return_value = False

            # act
            errors = self.plugin._run_linter(self.test_file)

            # assert
            self.assertEqual(errors, [])

    def test_should_return_cached_results(self) -> None:
        """Test that cached results are returned for previously linted files."""
        # arrange
        self.plugin._linter_results[self.test_file] = self.error_data

        # act
        errors = self.plugin._run_linter(self.test_file)

        # assert
        self.assertEqual(errors, self.error_data)

    def test_should_return_empty_for_ignored_paths(self) -> None:
        """Test that ignored paths return empty results."""
        # arrange/act/assert
        self.assertEqual(self.plugin._run_linter(""), [])
        self.assertEqual(self.plugin._run_linter("site-packages/foo.py"), [])
        self.assertEqual(self.plugin._run_linter("foo.pyi"), [])

    def test_should_raise_error_when_extension_not_found(self) -> None:
        """Test that LinterNotFoundError is raised when extension cannot be imported."""
        # arrange
        new_plugin = PandasLinterPlugin(Options())

        with (
            patch("typedframes_lint.mypy.get_project_root") as mock_root,
            patch("typedframes_lint.mypy.is_enabled") as mock_enabled,
        ):
            mock_enabled.return_value = True
            mock_root.return_value = Path()

            # Remove the rust linter from modules to simulate import failure
            with patch.dict(sys.modules, {"typedframes_lint._rust_linter": None}):
                # act/assert
                with self.assertRaises(LinterNotFoundError) as ctx:
                    new_plugin._run_linter(self.test_file)

                self.assertIn("typedframes linter extension not found", str(ctx.exception))

    def test_should_not_report_error_when_line_is_far_from_errors(self) -> None:
        """Test that no error is reported when access line is far from all error lines."""
        # arrange
        far_error_data = [{"line": 100, "message": "Column 'bar' does not exist"}]
        mock_check_file = MagicMock(return_value=json.dumps(far_error_data))
        new_plugin = PandasLinterPlugin(Options())

        with (
            patch("typedframes_lint.mypy.get_project_root") as mock_root,
            patch("typedframes_lint.mypy.is_enabled") as mock_enabled,
            patch.dict(sys.modules, {"typedframes_lint._rust_linter": MagicMock(check_file=mock_check_file)}),
        ):
            mock_enabled.return_value = True
            mock_root.return_value = Path()

            context = MagicMock()
            context.api.path = "far_test.py"
            context.context.line = 10  # Far from error line 100

            # act
            new_plugin.check_column_access(context)

            # assert — no fail called since line 10 is far from line 100
            context.api.fail.assert_not_called()

    def test_should_get_method_hook(self) -> None:
        """Test that get_method_hook returns correct hooks for DataFrame methods."""
        # arrange/act/assert
        self.assertEqual(
            self.plugin.get_method_hook("pandas.core.frame.DataFrame.__getitem__"),
            self.plugin.check_column_access,
        )
        self.assertEqual(
            self.plugin.get_method_hook("pandas.core.frame.DataFrame.__setitem__"),
            self.plugin.check_column_access,
        )
        self.assertIsNone(self.plugin.get_method_hook("other"))

    def test_should_return_plugin_class(self) -> None:
        """Test that plugin function returns the TypedFramesPlugin class."""
        # arrange/act
        result = plugin("1.0")

        # assert
        self.assertEqual(result, PandasLinterPlugin)

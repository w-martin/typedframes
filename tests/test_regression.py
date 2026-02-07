"""Regression tests for typedframes mypy plugin integration."""

import unittest

from mypy.api import run as mypy_run


class TestPluginRegression(unittest.TestCase):
    """Regression tests for the mypy plugin."""

    def test_should_not_catch_errors_without_plugin(self) -> None:
        """Test that mypy alone doesn't catch column errors."""
        # arrange
        test_file = "tests/fixtures/missing_column.py"

        # act - run mypy without the plugin
        stdout, _stderr, _exit_code = mypy_run(
            [
                "--ignore-missing-imports",
                "--no-error-summary",
                "--config-file",
                "/dev/null",  # Ignore pyproject.toml to skip plugin
                test_file,
            ]
        )

        # assert
        self.assertNotIn("Column 'non_existent' does not exist", stdout)

    def test_should_catch_errors_with_plugin(self) -> None:
        """Test that mypy with the plugin catches column errors."""
        # arrange
        test_file = "tests/fixtures/missing_column.py"

        # act - run mypy with pyproject.toml config (includes plugin)
        stdout, _stderr, exit_code = mypy_run([test_file])

        # assert
        self.assertIn("Column 'non_existent' does not exist in UserSchema", stdout)
        self.assertEqual(exit_code, 1)

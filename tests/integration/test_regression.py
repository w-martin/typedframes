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

    def test_should_accept_polarsframe_with_type_argument(self) -> None:
        """Test that mypy accepts PolarsFrame[Schema] without type-arg errors."""
        # arrange
        test_file = "tests/fixtures/polarsframe_generic.py"

        # act
        stdout, _stderr, _exit_code = mypy_run(
            [
                "--ignore-missing-imports",
                "--no-error-summary",
                "--config-file",
                "/dev/null",
                test_file,
            ]
        )

        # assert - the type-arg error should not appear
        self.assertNotIn("type-arg", stdout, f"Unexpected type-arg error in mypy output: {stdout}")

    def test_should_catch_errors_with_plugin(self) -> None:
        """Test that mypy with the plugin catches column errors."""
        # arrange
        test_file = "tests/fixtures/missing_column.py"
        # Use a stable fixture path (rather than a fresh tempfile per run) so mypy's
        # incremental cache can be reused across test runs instead of paying a full
        # cold typeshed/stdlib check every time. A dedicated cache dir keeps this from
        # colliding with test_should_not_catch_errors_without_plugin, which checks the
        # same file without the plugin enabled - sharing a cache dir would make each
        # test invalidate the other's entry for that file on every run.
        cfg_path = "tests/fixtures/mypy_with_plugin.ini"

        # act - run mypy with plugin configured via fixture config (avoids relying on pyproject.toml)
        stdout, _stderr, exit_code = mypy_run(
            [
                "--no-error-summary",
                "--config-file",
                cfg_path,
                "--cache-dir",
                ".mypy_cache/plugin_regression_test",
                test_file,
            ]
        )

        # assert
        self.assertIn("Column 'non_existent' does not exist in UserSchema", stdout)
        self.assertEqual(exit_code, 1)

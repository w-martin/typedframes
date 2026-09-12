"""Regression tests for typedframes mypy plugin integration."""

import unittest

from mypy.api import run as mypy_run


class TestPluginRegression(unittest.TestCase):
    """Regression tests for the mypy plugin."""

    def _run_mypy(self, *args: str) -> tuple[str, str, int]:
        """Run mypy in-process against a cache directory dedicated to the calling test.

        Every test in this class gets its own cache dir, and goes through this helper so
        that a new test cannot accidentally fall back to the shared default one.

        The isolation matters because the suite runs under pytest-xdist, so two of these
        tests can be running mypy in separate worker processes at the same time. When
        mypy opens its sqlite incremental cache it switches the database journal to WAL,
        which needs an exclusive lock and fails outright - it does not wait on the busy
        timeout - if another mypy process already holds one. Sharing a cache dir
        therefore surfaces as an intermittent `sqlite3.OperationalError: database is
        locked` on whichever test loses the race. Sharing also makes the tests that check
        the same fixture file with and without the plugin invalidate each other's entry
        for that file on every run.

        The path is derived from the test name rather than a fresh temporary directory
        per run, so mypy's incremental cache is still reused between runs instead of
        paying for a full cold typeshed/stdlib check every time.

        Args:
            args: Command line arguments to pass to mypy, including the file to check.

        Returns:
            The mypy stdout, stderr and exit code.
        """
        return mypy_run(["--cache-dir", f".mypy_cache/{self._testMethodName}", *args])

    def test_should_not_catch_errors_without_plugin(self) -> None:
        """Test that mypy alone doesn't catch column errors."""
        # arrange
        test_file = "tests/fixtures/missing_column.py"

        # act - run mypy without the plugin
        stdout, _stderr, _exit_code = self._run_mypy(
            "--ignore-missing-imports",
            "--no-error-summary",
            "--config-file",
            "/dev/null",  # Ignore pyproject.toml to skip plugin
            test_file,
        )

        # assert
        self.assertNotIn("Column 'non_existent' does not exist", stdout)

    def test_should_accept_polarsframe_with_type_argument(self) -> None:
        """Test that mypy accepts PolarsFrame[Schema] without type-arg errors."""
        # arrange
        test_file = "tests/fixtures/polarsframe_generic.py"

        # act
        stdout, _stderr, _exit_code = self._run_mypy(
            "--ignore-missing-imports",
            "--no-error-summary",
            "--config-file",
            "/dev/null",
            test_file,
        )

        # assert - the type-arg error should not appear
        self.assertNotIn("type-arg", stdout, f"Unexpected type-arg error in mypy output: {stdout}")

    def test_should_catch_errors_with_plugin(self) -> None:
        """Test that mypy with the plugin catches column errors."""
        # arrange
        test_file = "tests/fixtures/missing_column.py"
        cfg_path = "tests/fixtures/mypy_with_plugin.ini"

        # act - run mypy with plugin configured via fixture config (avoids relying on pyproject.toml)
        stdout, _stderr, exit_code = self._run_mypy(
            "--no-error-summary",
            "--config-file",
            cfg_path,
            test_file,
        )

        # assert
        self.assertIn("Column 'non_existent' does not exist in UserSchema", stdout)
        self.assertEqual(exit_code, 1)

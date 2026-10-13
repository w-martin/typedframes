"""Unit tests for the mypy plugin."""

import json
import unittest
from pathlib import Path
from subprocess import CompletedProcess
from unittest.mock import MagicMock, patch

from mypy.options import Options

from typedframes._binary import BinaryNotFoundError
from typedframes.mypy import CheckerFailedError, CheckerNotFoundError, TypedFramesPlugin, plugin


def _completed(errors: list[dict], *, returncode: int = 0, stderr: str = "") -> CompletedProcess[str]:
    """What the executable returns for a project: its JSON document, or a failure."""
    stdout = json.dumps({"errors": errors, "stats": {"dataframes_total": 0, "dataframes_typed": 0}})
    return CompletedProcess(args=[], returncode=returncode, stdout=stdout, stderr=stderr)


class TestTypedFramesPluginUnit(unittest.TestCase):
    """Unit tests for the TypedFramesPlugin class."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        # arrange
        self.plugin = TypedFramesPlugin(Options())
        self.test_file = "test.py"
        self.test_path = str(Path(self.test_file).resolve())
        self.error_data = [{"line": 10, "message": "Column 'foo' does not exist", "file": self.test_path}]

    def _context(self, path: str | None, line: int) -> MagicMock:
        context = MagicMock()
        context.api.path = path
        context.context.line = line
        return context

    def _check(self, plugin_: TypedFramesPlugin, errors: list[dict], path: str, line: int) -> MagicMock:
        """Run `check_column_access` for `path` at `line` with the executable reporting `errors`."""
        context = self._context(path, line)
        with (
            patch("typedframes.mypy.get_project_root", return_value=Path()),
            patch("typedframes.mypy.is_enabled", return_value=True),
            patch("typedframes.mypy.run_binary", return_value=_completed(errors)),
        ):
            plugin_.check_column_access(context)
        return context

    def test_should_report_error_on_exact_line_match(self) -> None:
        """Test that errors are reported on exact line matches."""
        # arrange / act
        context = self._check(self.plugin, self.error_data, self.test_file, 10)

        # assert
        context.api.fail.assert_called_once_with(self.error_data[0]["message"], context.context)

    def test_should_not_report_errors_belonging_to_another_file(self) -> None:
        """Test that a file only gets its own diagnostics."""
        # arrange / act
        context = self._check(self.plugin, self.error_data, "other.py", 10)

        # assert
        context.api.fail.assert_not_called()

    def test_should_report_error_on_fuzzy_line_match(self) -> None:
        """Test that errors are reported on fuzzy line matches within tolerance."""
        # arrange / act
        context = self._check(self.plugin, self.error_data, self.test_file, 11)

        # assert
        context.api.fail.assert_called_once_with(self.error_data[0]["message"], context.context)

    def test_should_not_report_error_when_line_is_far_from_errors(self) -> None:
        """Test that no error is reported when access line is far from all error lines."""
        # arrange
        far = [{"line": 100, "message": "Column 'bar' does not exist", "file": self.test_path}]

        # act
        context = self._check(self.plugin, far, self.test_file, 10)

        # assert
        context.api.fail.assert_not_called()

    def test_should_return_default_when_no_path(self) -> None:
        """Test that default return type is used when no path is available."""
        # arrange
        context = self._context(None, 1)
        context.default_return_type = MagicMock()

        # act
        result = self.plugin.check_column_access(context)

        # assert
        self.assertEqual(result, context.default_return_type)

    def test_should_not_run_when_disabled(self) -> None:
        """Test that checker does not run when disabled in config."""
        # arrange
        with (
            patch("typedframes.mypy.is_enabled", return_value=False),
            patch("typedframes.mypy.run_binary") as run,
        ):
            # act
            errors = self.plugin._run_checker(self.test_file)

        # assert
        self.assertEqual(errors, [])
        run.assert_not_called()

    def test_should_return_empty_for_ignored_paths(self) -> None:
        """Test that ignored paths return empty results."""
        # arrange/act/assert
        self.assertEqual(self.plugin._run_checker(""), [])
        self.assertEqual(self.plugin._run_checker("site-packages/foo.py"), [])
        self.assertEqual(self.plugin._run_checker("foo.pyi"), [])

    def test_should_run_the_checker_once_per_project_root(self) -> None:
        """Test that every file of a project is served from one run of the executable."""
        # arrange
        errors = [
            {"line": 1, "message": "a", "file": str(Path("a.py").resolve())},
            {"line": 2, "message": "b", "file": str(Path("b.py").resolve())},
        ]

        with (
            patch("typedframes.mypy.get_project_root", return_value=Path("/some/project")),
            patch("typedframes.mypy.is_enabled", return_value=True),
            patch("typedframes.mypy.run_binary", return_value=_completed(errors)) as run,
        ):
            # act
            first = self.plugin._run_checker("a.py")
            second = self.plugin._run_checker("b.py")

        # assert
        run.assert_called_once_with(["check", "/some/project", "--output-format", "json"], capture=True)
        self.assertEqual([errors[0]], first)
        self.assertEqual([errors[1]], second)

    def test_should_work_out_a_files_answer_once(self) -> None:
        """Test that the hook, which runs for every subscript, does not redo the project lookup."""
        # arrange
        with (
            patch("typedframes.mypy.get_project_root", return_value=Path()) as root,
            patch("typedframes.mypy.is_enabled", return_value=True) as enabled,
            patch("typedframes.mypy.run_binary", return_value=_completed(self.error_data)),
        ):
            # act
            first = self.plugin._run_checker(self.test_file)
            second = self.plugin._run_checker(self.test_file)

        # assert
        self.assertEqual(first, second)
        root.assert_called_once()
        enabled.assert_called_once()

    def test_should_serve_a_root_inside_an_already_checked_one_from_that_run(self) -> None:
        """Test that nested directories without a project file don't each run the checker again."""
        # arrange
        errors = [{"line": 1, "message": "a", "file": "/proj/sub/a.py"}]
        roots = iter([Path("/proj"), Path("/proj/sub"), Path("/elsewhere")])

        with (
            patch("typedframes.mypy.get_project_root", side_effect=lambda _path: next(roots)),
            patch("typedframes.mypy.is_enabled", return_value=True),
            patch("typedframes.mypy.run_binary", return_value=_completed(errors)) as run,
        ):
            # act
            self.plugin._run_checker("/proj/x.py")
            nested = self.plugin._run_checker("/proj/sub/a.py")
            self.plugin._run_checker("/elsewhere/b.py")

        # assert
        self.assertEqual(2, run.call_count)
        self.assertEqual(errors, nested)

    def test_should_raise_error_when_the_executable_is_not_found(self) -> None:
        """Test that CheckerNotFoundError is raised when the executable is not installed."""
        # arrange
        with (
            patch("typedframes.mypy.get_project_root", return_value=Path()),
            patch("typedframes.mypy.is_enabled", return_value=True),
            patch("typedframes.mypy.run_binary", side_effect=BinaryNotFoundError("missing")),
            self.assertRaises(CheckerNotFoundError) as ctx,
        ):
            # act
            self.plugin._run_checker(self.test_file)

        # assert
        self.assertIn("typedframes checker executable not found", str(ctx.exception))

    def test_should_raise_error_when_the_executable_fails(self) -> None:
        """Test that a failing run is reported with the executable's own message."""
        # arrange
        failed = _completed([], returncode=2, stderr="Error: boom\n")
        with (
            patch("typedframes.mypy.get_project_root", return_value=Path()),
            patch("typedframes.mypy.is_enabled", return_value=True),
            patch("typedframes.mypy.run_binary", return_value=failed),
            self.assertRaises(CheckerFailedError) as ctx,
        ):
            # act
            self.plugin._run_checker(self.test_file)

        # assert
        self.assertIn("status 2: Error: boom", str(ctx.exception))

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
        self.assertEqual(result, TypedFramesPlugin)

"""Unit tests for finding and running the typedframes executable."""

import os
import sys
from pathlib import Path
from subprocess import CompletedProcess
from tempfile import TemporaryDirectory
from unittest import TestCase
from unittest.mock import MagicMock, patch

from typedframes._binary import IN_PROCESS_VARIABLE, BinaryNotFoundError, find_binary, main, main_in_process, run_binary


def _touch(directory: str, *parts: str) -> Path:
    file = Path(directory, *parts)
    file.parent.mkdir(parents=True, exist_ok=True)
    file.write_text("")
    return file


class TestFindBinary(TestCase):
    """Where the executable is looked for, and in what order."""

    def _find(self, scripts: str, user: str, path_hit: str | None = None) -> Path:
        def get_path(_name: str, scheme: str | None = None) -> str:
            return user if scheme == "user-scheme" else scripts

        with (
            patch("typedframes._binary.get_path", get_path),
            patch("typedframes._binary.get_preferred_scheme", return_value="user-scheme"),
            patch("typedframes._binary.get_config_var", return_value=""),
            patch("typedframes._binary.shutil.which", return_value=path_hit),
        ):
            return find_binary()

    def test_should_prefer_the_interpreters_scripts_directory(self) -> None:
        """Test that the scripts directory of the running environment wins."""
        # arrange
        with TemporaryDirectory() as scripts, TemporaryDirectory() as user:
            expected = _touch(scripts, "typedframes")
            _touch(user, "typedframes")

            # act
            found = self._find(scripts, user)

        # assert
        self.assertEqual(expected, found)

    def test_should_fall_back_to_the_user_scripts_directory(self) -> None:
        """Test that a user-scheme install is found when the environment has none."""
        # arrange
        with TemporaryDirectory() as scripts, TemporaryDirectory() as user:
            expected = _touch(user, "typedframes")

            # act
            found = self._find(scripts, user)

        # assert
        self.assertEqual(expected, found)

    def test_should_fall_back_to_a_bin_directory_beside_the_package(self) -> None:
        """Test the layout `pip install --target` makes."""
        # arrange
        package_root = Path(sys.modules[find_binary.__module__].__file__ or "").resolve().parents[1]
        target = package_root / "bin" / "typedframes"
        with (
            TemporaryDirectory() as scripts,
            TemporaryDirectory() as user,
            patch.object(Path, "is_file", lambda p: p == target),
        ):
            # act
            found = self._find(scripts, user)

        # assert
        self.assertEqual(target, found)

    def test_should_fall_back_to_the_path(self) -> None:
        """Test that an executable on PATH is used when no installed location holds one."""
        # arrange
        with TemporaryDirectory() as scripts, TemporaryDirectory() as user:
            # act
            found = self._find(scripts, user, path_hit="/usr/local/bin/typedframes")

        # assert
        self.assertEqual(Path("/usr/local/bin/typedframes"), found)

    def test_should_say_where_it_looked_when_there_is_no_executable(self) -> None:
        """Test the error for an install without the executable."""
        # arrange
        with (
            TemporaryDirectory() as scripts,
            TemporaryDirectory() as user,
            self.assertRaises(BinaryNotFoundError) as ctx,
        ):
            # act
            self._find(scripts, user)

        # assert
        self.assertIn("was not found (looked for", str(ctx.exception))
        self.assertIn("uv run inv build", str(ctx.exception))


class TestRunBinary(TestCase):
    """Running the executable and the `python -m typedframes` entry point."""

    def test_should_run_the_executable_with_the_given_arguments(self) -> None:
        """Test that arguments are passed through and output is captured on request."""
        # arrange
        completed = CompletedProcess(args=[], returncode=0, stdout="out", stderr="")
        with (
            patch("typedframes._binary.find_binary", return_value=Path("/bin/typedframes")),
            patch("typedframes._binary.subprocess.run", return_value=completed) as run,
        ):
            # act
            result = run_binary(["check", "."], capture=True)

        # assert
        run.assert_called_once_with(["/bin/typedframes", "check", "."], capture_output=True, text=True, check=False)
        self.assertEqual(completed, result)

    def test_should_replace_this_process_with_the_executable_off_windows(self) -> None:
        """Test that `python -m typedframes` hands over to the executable where the platform can."""
        # arrange
        with (
            patch.object(sys, "platform", "linux"),
            patch.object(sys, "argv", ["typedframes", "check", "."]),
            patch("typedframes._binary.find_binary", return_value=Path("/bin/typedframes")),
            patch("typedframes._binary.os.execv") as execv,
        ):
            # act
            main()

        # assert
        execv.assert_called_once_with("/bin/typedframes", ["/bin/typedframes", "check", "."])

    def test_should_exit_with_the_executables_status_on_windows(self) -> None:
        """Test that Windows, which cannot replace a process, runs the executable and returns its status."""
        # arrange
        completed = CompletedProcess(args=[], returncode=3, stdout="", stderr="")
        with (
            patch.object(sys, "platform", "win32"),
            patch.object(sys, "argv", ["typedframes", "check", "."]),
            patch("typedframes._binary.run_binary", return_value=completed) as run,
            self.assertRaises(SystemExit) as ctx,
        ):
            # act
            main()

        # assert
        run.assert_called_once_with(["check", "."])
        self.assertEqual(3, ctx.exception.code)


def _extension(status: int = 0, stdout: str = "", stderr: str = "") -> MagicMock:
    extension = MagicMock()
    extension.main.return_value = status
    extension.run_captured.return_value = (status, stdout, stderr)
    return extension


class TestInProcess(TestCase):
    """Running the command line inside this process, through the extension module."""

    def test_should_run_the_extension_when_the_environment_asks_for_it(self) -> None:
        """Test that `TYPEDFRAMES_INPROC` runs in this process and never looks for the executable."""
        # arrange
        extension = _extension(status=3)
        with (
            patch.dict(os.environ, {IN_PROCESS_VARIABLE: "1"}),
            patch("typedframes._binary.find_binary", side_effect=AssertionError("started the executable")),
            patch("typedframes._binary.import_module", return_value=extension),
        ):
            # act
            result = run_binary(["check", "."])

        # assert
        extension.main.assert_called_once_with(["typedframes", "check", "."])
        self.assertEqual(3, result.returncode)

    def test_should_return_the_captured_output_of_the_extension(self) -> None:
        """Test that `capture` gives back the extension's standard output."""
        # arrange
        extension = _extension(status=2, stdout="{}", stderr="Error: path does not exist")
        with (
            patch.dict(os.environ, {IN_PROCESS_VARIABLE: "1"}),
            patch("typedframes._binary.import_module", return_value=extension),
        ):
            # act
            result = run_binary(["check", "."], capture=True)

        # assert
        extension.run_captured.assert_called_once_with(["typedframes", "check", "."])
        self.assertEqual("{}", result.stdout)
        self.assertEqual("Error: path does not exist", result.stderr)
        self.assertEqual(2, result.returncode)

    def test_should_start_the_executable_when_the_variable_says_off(self) -> None:
        """Test that `0`, `false`, `no` and `off`, in any case, are the same as unset."""
        for value in ("0", "false", "No", " OFF "):
            with self.subTest(value=value):
                self._run_with_the_executable(value)

    def _run_with_the_executable(self, value: str) -> None:
        completed = CompletedProcess(args=[], returncode=0, stdout="", stderr="")
        with (
            patch.dict(os.environ, {IN_PROCESS_VARIABLE: value}),
            patch("typedframes._binary.find_binary", return_value=Path("/bin/typedframes")),
            patch("typedframes._binary.import_module", side_effect=AssertionError("loaded the extension")),
            patch("typedframes._binary.subprocess.run", return_value=completed) as run,
        ):
            # act
            run_binary(["check", "."])

        # assert
        run.assert_called_once()

    def test_should_use_the_extension_when_there_is_no_executable(self) -> None:
        """Test the fallback for an install that has the extension but not the executable."""
        # arrange
        extension = _extension()
        with (
            patch.dict(os.environ, clear=True),
            patch("typedframes._binary.find_binary", side_effect=BinaryNotFoundError),
            patch("typedframes._binary.import_module", return_value=extension),
        ):
            # act
            run_binary(["check", "."])

        # assert
        extension.main.assert_called_once_with(["typedframes", "check", "."])

    def test_should_report_the_missing_executable_when_there_is_no_extension_either(self) -> None:
        """Test that an install with neither raises the executable's error."""
        # arrange
        with (
            patch.dict(os.environ, clear=True),
            patch("typedframes._binary.find_binary", side_effect=BinaryNotFoundError("no executable")),
            patch("typedframes._binary.import_module", side_effect=ImportError),
            self.assertRaises(BinaryNotFoundError) as ctx,
        ):
            # act
            run_binary(["check", "."])

        # assert
        self.assertEqual("no executable", str(ctx.exception))

    def test_should_say_so_when_the_extension_was_asked_for_but_is_not_installed(self) -> None:
        """Test the error for `TYPEDFRAMES_INPROC` without the extension module."""
        # arrange
        with (
            patch.dict(os.environ, {IN_PROCESS_VARIABLE: "1"}),
            patch("typedframes._binary.import_module", side_effect=ImportError),
            self.assertRaises(BinaryNotFoundError) as ctx,
        ):
            # act
            run_binary(["check", "."])

        # assert
        self.assertIn("typedframes._rust", str(ctx.exception))

    def test_should_exit_with_the_extensions_status_from_the_module_entry_point(self) -> None:
        """Test that `python -m typedframes` runs in this process, not as a replacement, when asked."""
        # arrange
        extension = _extension(status=2)
        with (
            patch.dict(os.environ, {IN_PROCESS_VARIABLE: "1"}),
            patch.object(sys, "platform", "linux"),
            patch.object(sys, "argv", ["typedframes", "check", "."]),
            patch("typedframes._binary.import_module", return_value=extension),
            patch("typedframes._binary.os.execv", side_effect=AssertionError("started the executable")),
            self.assertRaises(SystemExit) as ctx,
        ):
            # act
            main()

        # assert
        self.assertEqual(2, ctx.exception.code)

    def test_should_run_in_process_from_the_in_process_entry_point_without_the_variable(self) -> None:
        """Test that `typedframes-py` never starts the executable, whatever the environment says."""
        # arrange
        extension = _extension(status=4)
        with (
            patch.dict(os.environ, {IN_PROCESS_VARIABLE: ""}),
            patch.object(sys, "argv", ["typedframes-py", "check", "."]),
            patch("typedframes._binary.find_binary", side_effect=AssertionError("started the executable")),
            patch("typedframes._binary.import_module", return_value=extension),
            self.assertRaises(SystemExit) as ctx,
        ):
            # act
            main_in_process()

        # assert
        extension.main.assert_called_once_with(["typedframes", "check", "."])
        self.assertEqual(4, ctx.exception.code)

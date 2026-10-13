"""Unit tests for finding and running the typedframes executable."""

import sys
from pathlib import Path
from subprocess import CompletedProcess
from tempfile import TemporaryDirectory
from unittest import TestCase
from unittest.mock import patch

from typedframes._binary import BinaryNotFoundError, find_binary, main, run_binary


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

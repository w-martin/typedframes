"""Unit tests for the typedframes CLI."""

import builtins
import json
import tempfile
import unittest
from io import StringIO
from pathlib import Path
from unittest.mock import patch

from typedframes.cli import _check_files, _collect_python_files, _format_human, main


class TestCli(unittest.TestCase):
    """Unit tests for the CLI entry point."""

    def test_should_print_help_when_no_command(self) -> None:
        """Test that running with no arguments prints help and exits 2."""
        # arrange / act / assert
        with self.assertRaises(SystemExit) as ctx:
            main([])
        self.assertEqual(ctx.exception.code, 2)

    def test_should_exit_2_for_nonexistent_path(self) -> None:
        """Test that a nonexistent path exits with code 2."""
        # arrange / act / assert
        with self.assertRaises(SystemExit) as ctx:
            main(["check", "/nonexistent/path/xyz"])
        self.assertEqual(ctx.exception.code, 2)

    def test_should_exit_1_when_checker_not_installed(self) -> None:
        """Test that a helpful error is shown when typedframes-checker is missing."""
        # arrange
        original_import = builtins.__import__

        def mock_import(name: str, *args: object, **kwargs: object) -> object:
            if name == "typedframes_checker._rust_checker":
                raise ImportError(name)
            return original_import(name, *args, **kwargs)

        with tempfile.TemporaryDirectory() as tmpdir:
            py_file = Path(tmpdir) / "test.py"
            py_file.write_text("x = 1\n")

            captured = StringIO()

            # act / assert
            with (
                patch("builtins.__import__", side_effect=mock_import),
                patch("sys.stderr", captured),
                self.assertRaises(SystemExit) as ctx,
            ):
                _check_files([py_file])

            self.assertEqual(ctx.exception.code, 1)
            self.assertIn("requires the typedframes-checker package", captured.getvalue())

    def test_should_collect_single_python_file(self) -> None:
        """Test collecting a single .py file."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            py_file = Path(tmpdir) / "test.py"
            py_file.write_text("x = 1")

            # act
            result = _collect_python_files(py_file)

            # assert
            self.assertEqual(result, [py_file])

    def test_should_skip_non_python_file(self) -> None:
        """Test that non-.py files are skipped."""
        # arrange
        import tempfile

        with tempfile.TemporaryDirectory() as tmpdir:
            txt_file = Path(tmpdir) / "data.txt"
            txt_file.write_text("hello")

            # act
            result = _collect_python_files(txt_file)

            # assert
            self.assertEqual(result, [])

    def test_should_collect_python_files_from_directory(self) -> None:
        """Test recursive collection of .py files from a directory."""
        # arrange
        import tempfile

        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            (root / "a.py").write_text("x = 1")
            (root / "b.txt").write_text("hello")
            sub = root / "sub"
            sub.mkdir()
            (sub / "c.py").write_text("y = 2")

            # act
            result = _collect_python_files(root)

            # assert
            self.assertEqual(len(result), 2)
            names = [f.name for f in result]
            self.assertIn("a.py", names)
            self.assertIn("c.py", names)

    def test_should_format_human_readable_errors(self) -> None:
        """Test human-readable error formatting."""
        # arrange
        errors = [
            {"file": "src/foo.py", "line": 23, "col": 0, "message": "Column 'x' not in Schema"},
            {"file": "src/bar.py", "line": 10, "col": 0, "message": "Column 'y' not in Schema"},
        ]

        # act
        result = _format_human(errors)

        # assert
        self.assertIn("\u2717 src/foo.py:23 - Column 'x' not in Schema", result)
        self.assertIn("\u2717 src/bar.py:10 - Column 'y' not in Schema", result)

    def test_should_output_json_when_flag_set(self) -> None:
        """Test JSON output mode."""
        # arrange
        import tempfile

        with tempfile.TemporaryDirectory() as tmpdir:
            py_file = Path(tmpdir) / "clean.py"
            py_file.write_text("x = 1\n")

            captured = StringIO()

            # act
            with patch("sys.stdout", captured):
                main(["check", str(py_file), "--json"])

            # assert
            output = captured.getvalue()
            parsed = json.loads(output)
            self.assertIsInstance(parsed, list)

    def test_should_exit_0_when_strict_and_no_errors(self) -> None:
        """Test that --strict exits 0 when there are no errors."""
        # arrange
        import tempfile

        with tempfile.TemporaryDirectory() as tmpdir:
            py_file = Path(tmpdir) / "clean.py"
            py_file.write_text("x = 1\n")

            # act / assert — should not raise SystemExit
            main(["check", str(py_file), "--strict"])

    def test_should_exit_1_when_strict_and_errors(self) -> None:
        """Test that --strict exits 1 when there are errors."""
        # arrange
        import tempfile

        with tempfile.TemporaryDirectory() as tmpdir:
            py_file = Path(tmpdir) / "bad.py"
            py_file.write_text(
                "from typedframes import BaseSchema, Column\n"
                "\n"
                "class S(BaseSchema):\n"
                "    x = Column(type=int)\n"
                "\n"
                'df: "DataFrame[S]" = load()\n'
                'df["wrong"]\n'
            )

            # act / assert
            with self.assertRaises(SystemExit) as ctx:
                main(["check", str(py_file), "--strict"])
            self.assertEqual(ctx.exception.code, 1)

    def test_should_print_summary_for_clean_files(self) -> None:
        """Test that a summary line is printed for clean files."""
        # arrange
        import tempfile

        with tempfile.TemporaryDirectory() as tmpdir:
            py_file = Path(tmpdir) / "clean.py"
            py_file.write_text("x = 1\n")

            captured = StringIO()

            # act
            with patch("sys.stdout", captured):
                main(["check", str(py_file)])

            # assert
            output = captured.getvalue()
            self.assertIn("\u2713 Checked 1 file", output)

    def test_should_print_error_count_for_bad_files(self) -> None:
        """Test that error count is printed for files with errors."""
        # arrange
        import tempfile

        with tempfile.TemporaryDirectory() as tmpdir:
            py_file = Path(tmpdir) / "bad.py"
            py_file.write_text(
                "from typedframes import BaseSchema, Column\n"
                "\n"
                "class S(BaseSchema):\n"
                "    x = Column(type=int)\n"
                "\n"
                'df: "DataFrame[S]" = load()\n'
                'df["wrong"]\n'
            )

            captured = StringIO()

            # act
            with patch("sys.stdout", captured):
                main(["check", str(py_file)])

            # assert
            output = captured.getvalue()
            self.assertIn("\u2717 Found 1 error", output)

    def test_should_check_directory(self) -> None:
        """Test checking an entire directory."""
        # arrange
        import tempfile

        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            (root / "a.py").write_text("x = 1\n")
            (root / "b.py").write_text("y = 2\n")

            captured = StringIO()

            # act
            with patch("sys.stdout", captured):
                main(["check", str(root)])

            # assert
            output = captured.getvalue()
            self.assertIn("\u2713 Checked 2 files", output)

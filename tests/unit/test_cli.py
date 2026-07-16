"""Unit tests for the typedframes CLI."""

import builtins
import json
import tempfile
import unittest
from io import StringIO
from pathlib import Path
from unittest.mock import patch

from typedframes.cli import _check_files, _collect_python_files, _format_github, _format_text, main


class TestCli(unittest.TestCase):
    """Unit tests for the CLI entry point."""

    def test_should_print_help_when_no_command(self) -> None:
        """Test that running with no arguments prints help and exits 2."""
        # arrange / act / assert
        with self.assertRaises(SystemExit) as ctx:
            main([])
        self.assertEqual(ctx.exception.code, 2)

    def test_should_exit_2_for_nonexistent_path(self) -> None:
        """Test that a nonexistent absolute path exits with code 2."""
        # arrange / act / assert
        with self.assertRaises(SystemExit) as ctx:
            main(["check", "/nonexistent/path/xyz"])
        self.assertEqual(ctx.exception.code, 2)

    def test_should_show_resolved_path_for_nonexistent_relative_path(self) -> None:
        """Test that a nonexistent relative path shows both the original and resolved form."""
        # arrange
        captured = StringIO()

        # act / assert
        with (
            patch("sys.stderr", captured),
            self.assertRaises(SystemExit) as ctx,
        ):
            main(["check", "no/such/dir"])
        self.assertEqual(ctx.exception.code, 2)
        self.assertIn("'no/such/dir'", captured.getvalue())
        self.assertIn("resolved to", captured.getvalue())

    def test_should_exit_1_when_checker_not_installed(self) -> None:
        """Test that a helpful error is shown when typedframes-checker is missing."""
        # arrange
        original_import = builtins.__import__

        def mock_import(name: str, *args: object, **kwargs: object) -> object:
            if name == "typedframes._rust_checker":
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
            self.assertIn("Rust checker extension was not found", captured.getvalue())

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

    def test_should_format_text_errors(self) -> None:
        """Test text error formatting uses ty-style file:line:col: severity[code] message."""
        # arrange
        errors = [
            {
                "file": "src/foo.py",
                "line": 23,
                "col": 1,
                "code": "unknown-column",
                "message": "Column 'x' not in Schema",
                "severity": "error",
            },
            {
                "file": "src/bar.py",
                "line": 10,
                "col": 1,
                "code": "unknown-column",
                "message": "Column 'y' not in Schema",
                "severity": "error",
            },
        ]

        # act
        result = _format_text(errors)

        # assert
        self.assertIn("src/foo.py:23:1: error[unknown-column] Column 'x' not in Schema", result)
        self.assertIn("src/bar.py:10:1: error[unknown-column] Column 'y' not in Schema", result)

    def test_should_format_warning_with_severity_label(self) -> None:
        """Test that errors use 'error[code]' and warnings use 'warning[code]' labels."""
        # arrange
        items = [
            {
                "file": "a.py",
                "line": 1,
                "col": 4,
                "code": "unknown-column",
                "message": "error msg",
                "severity": "error",
            },
            {
                "file": "b.py",
                "line": 2,
                "col": 1,
                "code": "untracked-dataframe",
                "message": "warn msg",
                "severity": "warning",
            },
        ]

        # act
        result = _format_text(items)

        # assert
        self.assertIn("a.py:1:4: error[unknown-column] error msg", result)
        self.assertIn("b.py:2:1: warning[untracked-dataframe] warn msg", result)

    def test_should_format_text_with_color(self) -> None:
        """Test that color=True adds ANSI escape codes to the output."""
        # arrange
        errors = [
            {
                "file": "f.py",
                "line": 1,
                "col": 1,
                "code": "unknown-column",
                "message": "bad column",
                "severity": "error",
            },
        ]

        # act
        result = _format_text(errors, color=True)

        # assert — ANSI bold and red codes are present
        self.assertIn("\033[", result)
        self.assertIn("bad column", result)

    def test_should_format_github_annotations(self) -> None:
        """Test GitHub Actions annotation format."""
        # arrange
        errors = [
            {
                "file": "src/foo.py",
                "line": 42,
                "col": 8,
                "code": "unknown-column",
                "message": "Column 'x' not in Schema",
                "severity": "error",
            },
            {
                "file": "src/bar.py",
                "line": 10,
                "col": 1,
                "code": "untracked-dataframe",
                "message": "columns unknown",
                "severity": "warning",
            },
        ]

        # act
        result = _format_github(errors)

        # assert
        self.assertIn("::error file=src/foo.py,line=42,col=8,title=unknown-column::Column 'x' not in Schema", result)
        self.assertIn("::warning file=src/bar.py,line=10,col=1,title=untracked-dataframe::columns unknown", result)

    def test_should_output_json_when_flag_set(self) -> None:
        """Test JSON output mode via --json flag."""
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
            self.assertIsInstance(parsed["errors"], list)
            self.assertIn("dataframes_total", parsed["stats"])
            self.assertIn("dataframes_typed", parsed["stats"])

    def test_should_output_json_when_output_format_json(self) -> None:
        """Test JSON output mode via --output-format json."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            py_file = Path(tmpdir) / "clean.py"
            py_file.write_text("x = 1\n")

            captured = StringIO()

            # act
            with patch("sys.stdout", captured):
                main(["check", str(py_file), "--output-format", "json"])

            # assert
            output = captured.getvalue()
            parsed = json.loads(output)
            self.assertIsInstance(parsed["errors"], list)
            self.assertIn("dataframes_total", parsed["stats"])
            self.assertIn("dataframes_typed", parsed["stats"])

    def test_should_output_github_format(self) -> None:
        """Test GitHub Actions annotation output via --output-format github."""
        # arrange
        error = {
            "file": "f.py",
            "line": 5,
            "col": 4,
            "code": "unknown-column",
            "message": "Column 'x' not found",
            "severity": "error",
        }
        with tempfile.TemporaryDirectory() as tmpdir:
            py_file = Path(tmpdir) / "f.py"
            py_file.write_text("x = 1\n")

            captured = StringIO()

            # act
            with (
                patch(
                    "typedframes.cli._check_files",
                    return_value=([error], {"dataframes_total": 0, "dataframes_typed": 0}),
                ),
                patch("sys.stdout", captured),
            ):
                main(["check", str(py_file), "--output-format", "github"])

            # assert
            output = captured.getvalue()
            self.assertIn("::error file=f.py,line=5,col=4,title=unknown-column::Column 'x' not found", output)

    def test_should_output_github_format_clean_file(self) -> None:
        """Test GitHub Actions format with no errors produces no error/warning annotations."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            py_file = Path(tmpdir) / "clean.py"
            py_file.write_text("x = 1\n")

            captured = StringIO()

            # act
            with patch("sys.stdout", captured):
                main(["check", str(py_file), "--output-format", "github"])

            # assert — no error/warning annotations for a clean file, but the coverage
            # summary still prints as a "notice" (GitHub's closest tier to typedframes'
            # own low-key "info" severity)
            output = captured.getvalue()
            self.assertNotIn("::error", output)
            self.assertNotIn("::warning", output)
            self.assertIn("::notice title=typedframes coverage::", output)

    def test_should_suppress_github_notice_with_no_info_flag(self) -> None:
        """Test that --no-info suppresses the GitHub coverage notice annotation."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            py_file = Path(tmpdir) / "clean.py"
            py_file.write_text("x = 1\n")

            captured = StringIO()

            # act
            with patch("sys.stdout", captured):
                main(["check", str(py_file), "--output-format", "github", "--no-info"])

            # assert
            output = captured.getvalue()
            self.assertNotIn("::notice", output)

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

    def test_should_print_warning_count_in_summary(self) -> None:
        """Test that warning count appears in the summary line."""
        # arrange
        import tempfile

        with tempfile.TemporaryDirectory() as tmpdir:
            py_file = Path(tmpdir) / "warn.py"
            py_file.write_text("import pandas as pd\ndf = pd.read_csv('x.csv')\n")

            captured = StringIO()

            # act
            with patch("sys.stdout", captured):
                main(["check", str(py_file), "--no-index", "--strict-ingest"])

            # assert
            output = captured.getvalue()
            self.assertIn("1 warning", output)

    def test_should_not_exit_1_when_strict_and_only_warnings(self) -> None:
        """Test that --strict does not exit 1 when there are only warnings (no errors)."""
        # arrange
        import tempfile

        with tempfile.TemporaryDirectory() as tmpdir:
            py_file = Path(tmpdir) / "warn.py"
            py_file.write_text("import pandas as pd\ndf = pd.read_csv('x.csv')\n")

            # act / assert — should not raise SystemExit(1)
            main(["check", str(py_file), "--strict", "--no-index"])

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

    def test_should_check_directory_with_no_index(self) -> None:
        """Test that --no-index skips building the project index."""
        # arrange
        import tempfile

        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            (root / "a.py").write_text("x = 1\n")

            captured = StringIO()

            # act
            with patch("sys.stdout", captured):
                main(["check", str(root), "--no-index"])

            # assert
            output = captured.getvalue()
            self.assertIn("\u2713 Checked 1 file", output)

    def test_should_suppress_warnings_with_no_warnings_flag(self) -> None:
        """Test that --no-warnings suppresses untracked-dataframe/dropped-unknown-column warnings from output."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            py_file = Path(tmpdir) / "warn.py"
            py_file.write_text("import pandas as pd\ndf = pd.read_csv('x.csv')\n")

            captured = StringIO()

            # act
            with patch("sys.stdout", captured):
                main(["check", str(py_file), "--no-index", "--no-warnings"])

            # assert
            output = captured.getvalue()
            self.assertNotIn("warning", output)
            self.assertIn("\u2713 Checked 1 file", output)

    def test_should_still_show_errors_with_no_warnings_flag(self) -> None:
        """Test that --no-warnings suppresses warnings but preserves errors."""
        # arrange
        warning_error = {
            "file": "mixed.py",
            "line": 2,
            "col": 0,
            "code": "dropped-unknown-column",
            "message": "Dropped column 'x' does not exist in Schema",
            "severity": "warning",
        }
        actual_error = {
            "file": "mixed.py",
            "line": 7,
            "col": 0,
            "code": "unknown-column",
            "message": "Column 'wrong' not in Schema",
            "severity": "error",
        }

        with tempfile.TemporaryDirectory() as tmpdir:
            py_file = Path(tmpdir) / "mixed.py"
            py_file.write_text("x = 1\n")

            captured = StringIO()

            # act
            with (
                patch(
                    "typedframes.cli._check_files",
                    return_value=([warning_error, actual_error], {"dataframes_total": 0, "dataframes_typed": 0}),
                ),
                patch("sys.stdout", captured),
            ):
                main(["check", str(py_file), "--no-warnings"])

            # assert
            output = captured.getvalue()
            self.assertNotIn("Dropped column", output)
            self.assertIn("Column 'wrong'", output)
            self.assertIn("1 error", output)

    def test_should_show_untracked_dataframe_as_info_by_default(self) -> None:
        """Test that untracked-dataframe surfaces as a non-blocking info diagnostic by default."""
        # arrange
        w = {
            "file": "f.py",
            "line": 1,
            "col": 0,
            "code": "untracked-dataframe",
            "message": "columns unknown at lint time",
            "severity": "warning",
        }
        with tempfile.TemporaryDirectory() as tmpdir:
            py_file = Path(tmpdir) / "f.py"
            py_file.write_text("x = 1\n")
            captured = StringIO()

            # act
            with (
                patch(
                    "typedframes.cli._check_files",
                    return_value=([w], {"dataframes_total": 1, "dataframes_typed": 0}),
                ),
                patch("sys.stdout", captured),
            ):
                main(["check", str(py_file)])

            # assert \u2014 shown, but as a quiet "info" diagnostic, not a warning, and it
            # does not turn the pass/fail headline into a failure
            output = captured.getvalue()
            self.assertIn("columns unknown at lint time", output)
            self.assertIn("info[untracked-dataframe]", output)
            self.assertIn("\u2713 Checked 1 file", output)

    def test_should_escalate_untracked_dataframe_to_warning_with_strict_ingest_flag(self) -> None:
        """Test that --strict-ingest escalates untracked-dataframe from info to warning."""
        # arrange
        w = {
            "file": "f.py",
            "line": 1,
            "col": 0,
            "code": "untracked-dataframe",
            "message": "columns unknown at lint time",
            "severity": "warning",
        }
        with tempfile.TemporaryDirectory() as tmpdir:
            py_file = Path(tmpdir) / "f.py"
            py_file.write_text("x = 1\n")
            captured = StringIO()

            # act
            with (
                patch(
                    "typedframes.cli._check_files",
                    return_value=([w], {"dataframes_total": 1, "dataframes_typed": 0}),
                ),
                patch("sys.stdout", captured),
            ):
                main(["check", str(py_file), "--strict-ingest"])

            # assert
            output = captured.getvalue()
            self.assertIn("columns unknown at lint time", output)
            self.assertIn("warning[untracked-dataframe]", output)
            self.assertIn("1 warning", output)

    def test_should_show_dataframe_coverage_info_by_default(self) -> None:
        """Test that the DataFrame coverage summary line appears by default for a fully typed load."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            py_file = Path(tmpdir) / "typed.py"
            py_file.write_text("import pandas as pd\ndf = pd.read_csv('x.csv', usecols=['a', 'b'])\n")

            captured = StringIO()

            # act
            with patch("sys.stdout", captured):
                main(["check", str(py_file)])

            # assert
            output = captured.getvalue()
            self.assertIn("1/1 DataFrames had column info (100%)", output)

    def test_should_show_low_dataframe_coverage_for_untyped_load(self) -> None:
        """Test that a bare load without usecols/columns is reflected as 0% coverage."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            py_file = Path(tmpdir) / "untyped.py"
            py_file.write_text("import pandas as pd\ndf = pd.read_csv('x.csv')\n")

            captured = StringIO()

            # act
            with patch("sys.stdout", captured):
                main(["check", str(py_file)])

            # assert
            output = captured.getvalue()
            self.assertIn("0/1 DataFrames had column info (0%)", output)

    def test_should_suppress_info_output_with_no_info_flag(self) -> None:
        """Test that --no-info suppresses both the coverage line and info-level diagnostics."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            py_file = Path(tmpdir) / "untyped.py"
            py_file.write_text("import pandas as pd\ndf = pd.read_csv('x.csv')\n")

            captured = StringIO()

            # act
            with patch("sys.stdout", captured):
                main(["check", str(py_file), "--no-info"])

            # assert
            output = captured.getvalue()
            self.assertNotIn("DataFrames had column info", output)
            self.assertNotIn("columns unknown at lint time", output)
            self.assertIn("✓ Checked 1 file", output)

    def test_should_not_crash_when_checker_not_installed_on_directory(self) -> None:
        """Test that a missing Rust extension when checking a directory exits with code 1."""
        # arrange
        original_import = builtins.__import__

        def mock_import(name: str, *args: object, **kwargs: object) -> object:
            if name == "typedframes._rust_checker":
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
                main(["check", str(tmpdir)])

            self.assertEqual(ctx.exception.code, 1)
            self.assertIn("Rust checker extension was not found", captured.getvalue())

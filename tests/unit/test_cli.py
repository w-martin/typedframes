"""Unit tests for the typedframes CLI."""

import argparse
import builtins
import json
import tempfile
import unittest
from io import StringIO
from pathlib import Path
from unittest.mock import patch

from typedframes.cli import (
    CoverageBucket,
    CoverageConfig,
    RunStats,
    _check_files,
    _check_notebook_file,
    _collect_notebook_files,
    _collect_python_files,
    _coverage_json_payload,
    _coverage_message,
    _evaluate_coverage,
    _format_github,
    _format_term_missing,
    _format_text,
    _glob_to_regex,
    _load_configured_excludes,
    _load_coverage_config,
    _override_for,
    _percentage,
    _relative_posix,
    main,
)


def _write_notebook(path: Path, cells: list[str]) -> None:
    """Write a minimal valid .ipynb file with one code cell per entry in `cells`."""
    notebook = {
        "cells": [
            {
                "cell_type": "code",
                "metadata": {},
                "execution_count": None,
                "outputs": [],
                "source": source.splitlines(keepends=True),
            }
            for source in cells
        ],
        "metadata": {
            "kernelspec": {"display_name": "Python 3", "language": "python", "name": "python3"},
            "language_info": {"name": "python"},
        },
        "nbformat": 4,
        "nbformat_minor": 5,
    }
    path.write_text(json.dumps(notebook))


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

    def test_should_color_warning_severity_distinctly_from_error(self) -> None:
        """Test that color=True renders a warning severity with its own ANSI code."""
        # arrange
        errors = [
            {
                "file": "f.py",
                "line": 1,
                "col": 1,
                "code": "untracked-dataframe",
                "message": "columns unknown",
                "severity": "warning",
            },
        ]

        # act
        result = _format_text(errors, color=True)

        # assert
        self.assertIn("\033[1;33mwarning\033[0m", result)

    def test_should_color_info_severity_as_dim(self) -> None:
        """Test that color=True renders an info severity using the dim escape code."""
        # arrange
        errors = [
            {
                "file": "f.py",
                "line": 1,
                "col": 1,
                "code": "untracked-dataframe",
                "message": "columns unknown",
                "severity": "info",
            },
        ]

        # act
        result = _format_text(errors, color=True)

        # assert
        self.assertIn("\033[2minfo\033[0m", result)

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
            self.assertIn("::notice title=typedframes DataFrame schema coverage::", output)

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
                main(["check", str(py_file), "--no-index"])

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

    def test_should_show_untracked_dataframe_as_warning_by_default(self) -> None:
        """Test that untracked-dataframe surfaces as a warning diagnostic by default."""
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

            # assert -- shown as a warning, which turns the pass/fail headline into a
            # (non-strict) failure count
            output = captured.getvalue()
            self.assertIn("columns unknown at lint time", output)
            self.assertIn("warning[untracked-dataframe]", output)
            self.assertIn("1 warning", output)

    def test_should_downgrade_untracked_dataframe_to_info_with_lenient_ingest_flag(self) -> None:
        """Test that --lenient-ingest downgrades untracked-dataframe from warning to info."""
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
                main(["check", str(py_file), "--lenient-ingest"])

            # assert \u2014 shown, but as a quiet "info" diagnostic, not a warning, and it
            # does not turn the pass/fail headline into a failure
            output = captured.getvalue()
            self.assertIn("columns unknown at lint time", output)
            self.assertIn("info[untracked-dataframe]", output)
            self.assertIn("\u2713 Checked 1 file", output)

    def test_should_leave_non_untracked_dataframe_errors_untouched_by_lenient_ingest(self) -> None:
        """Test that --lenient-ingest only downgrades untracked-dataframe, leaving other codes alone."""
        # arrange
        e = {
            "file": "f.py",
            "line": 1,
            "col": 0,
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
                    return_value=([e], {"dataframes_total": 1, "dataframes_typed": 1}),
                ),
                patch("sys.stdout", captured),
            ):
                main(["check", str(py_file), "--lenient-ingest"])

            # assert -- unknown-column stays an error, unaffected by the ingest flag
            output = captured.getvalue()
            self.assertIn("error[unknown-column]", output)
            self.assertIn("1 error", output)

    def test_should_treat_strict_ingest_flag_as_a_harmless_noop(self) -> None:
        """Test that --strict-ingest is accepted but does nothing (untracked-dataframe is already a warning)."""
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

            # assert -- identical to the no-flag default
            output = captured.getvalue()
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

    def test_should_suppress_coverage_line_with_no_info_flag(self) -> None:
        """Test that --no-info suppresses the coverage line; untracked-dataframe (a warning) is unaffected."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            py_file = Path(tmpdir) / "untyped.py"
            py_file.write_text("import pandas as pd\ndf = pd.read_csv('x.csv')\n")

            captured = StringIO()

            # act
            with patch("sys.stdout", captured):
                main(["check", str(py_file), "--no-info"])

            # assert -- the coverage line is gone, but untracked-dataframe is a
            # warning by default, not info, so --no-info alone doesn't touch it
            output = captured.getvalue()
            self.assertNotIn("DataFrames had column info", output)
            self.assertIn("columns unknown at lint time", output)
            self.assertIn("1 warning", output)

    def test_should_suppress_info_output_with_lenient_ingest_and_no_info_flags(self) -> None:
        """Test that --lenient-ingest --no-info together suppress both the coverage line and the diagnostic."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            py_file = Path(tmpdir) / "untyped.py"
            py_file.write_text("import pandas as pd\ndf = pd.read_csv('x.csv')\n")

            captured = StringIO()

            # act
            with patch("sys.stdout", captured):
                main(["check", str(py_file), "--lenient-ingest", "--no-info"])

            # assert
            output = captured.getvalue()
            self.assertNotIn("DataFrames had column info", output)
            self.assertNotIn("columns unknown at lint time", output)
            self.assertIn("✓ Checked 1 file", output)

    def test_should_skip_excluded_vendor_directories_when_collecting(self) -> None:
        """Test that .venv/.git/__pycache__/node_modules subtrees are never descended into."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)

            venv_dir = root / ".venv" / "lib"
            venv_dir.mkdir(parents=True)
            (venv_dir / "fake.py").write_text("x = 1\n")

            git_dir = root / ".git" / "hooks"
            git_dir.mkdir(parents=True)
            (git_dir / "fake.py").write_text("x = 1\n")

            pycache_dir = root / "__pycache__"
            pycache_dir.mkdir(parents=True)
            (pycache_dir / "fake.py").write_text("x = 1\n")

            node_modules_dir = root / "node_modules" / "some-pkg"
            node_modules_dir.mkdir(parents=True)
            (node_modules_dir / "fake.py").write_text("x = 1\n")

            real_dir = root / "real"
            real_dir.mkdir()
            (real_dir / "app.py").write_text("x = 1\n")

            # act
            result = _collect_python_files(root)

            # assert
            names = [f.name for f in result]
            self.assertEqual(names, ["app.py"])

    def test_should_prune_dot_claude_by_default_with_no_excludes_given(self) -> None:
        """Test that .claude is pruned by the built-in default set with no config at all."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)

            claude_dir = root / ".claude" / "worktrees" / "agent-1"
            claude_dir.mkdir(parents=True)
            (claude_dir / "fake.py").write_text("x = 1\n")

            real_dir = root / "real"
            real_dir.mkdir()
            (real_dir / "app.py").write_text("x = 1\n")

            # act
            result = _collect_python_files(root)

            # assert
            names = [f.name for f in result]
            self.assertEqual(names, ["app.py"])

    def test_should_replace_rather_than_add_to_default_excludes_when_configured(self) -> None:
        """Test that configured_excludes REPLACES the built-in default set rather than adding to it."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)

            custom_dir = root / "custom_dir"
            custom_dir.mkdir()
            (custom_dir / "skipped.py").write_text("x = 1\n")

            venv_dir = root / ".venv"
            venv_dir.mkdir()
            (venv_dir / "walked.py").write_text("x = 1\n")

            # act -- configuring exclude WITHOUT re-listing .venv should let .venv be
            # walked again: override, not union.
            result = _collect_python_files(root, frozenset({"custom_dir"}))

            # assert
            names = [f.name for f in result]
            self.assertEqual(names, ["walked.py"])

    def test_should_not_prune_anything_extra_when_no_excludes_given(self) -> None:
        """Test that _collect_python_files with no configured_excludes only applies the built-in default set."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            custom_dir = root / "custom_vendor_dir"
            custom_dir.mkdir()
            (custom_dir / "fake.py").write_text("x = 1\n")

            # act
            result = _collect_python_files(root)

            # assert -- not in the built-in default set, so not pruned
            names = [f.name for f in result]
            self.assertEqual(names, ["fake.py"])

    def test_should_load_configured_excludes_from_pyproject_toml(self) -> None:
        """Test that _load_configured_excludes reads [tool.typedframes] exclude from pyproject.toml."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            (root / "pyproject.toml").write_text('[tool.typedframes]\nexclude = [".claude", "vendor"]\n')

            # act
            result = _load_configured_excludes(root)

            # assert
            self.assertEqual(result, frozenset({".claude", "vendor"}))

    def test_should_treat_an_explicitly_empty_exclude_list_as_prune_nothing(self) -> None:
        """Test that exclude = [] is a deliberate override (prune nothing), not "not configured"."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            (root / "pyproject.toml").write_text("[tool.typedframes]\nexclude = []\n")

            # act
            result = _load_configured_excludes(root)

            # assert -- an empty frozenset, not None
            self.assertEqual(result, frozenset())
            self.assertIsNotNone(result)

    def test_should_return_none_when_pyproject_toml_is_absent(self) -> None:
        """Test that _load_configured_excludes returns None (not configured) when there's no pyproject.toml."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)

            # act
            result = _load_configured_excludes(root)

            # assert
            self.assertIsNone(result)

    def test_should_return_none_when_path_is_a_file(self) -> None:
        """Test that _load_configured_excludes returns None for a single-file path."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            py_file = Path(tmpdir) / "test.py"
            py_file.write_text("x = 1\n")

            # act
            result = _load_configured_excludes(py_file)

            # assert
            self.assertIsNone(result)

    def test_should_return_none_when_pyproject_toml_is_malformed(self) -> None:
        """Test that a malformed pyproject.toml doesn't crash exclude-config loading."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            (root / "pyproject.toml").write_text("this is not [ valid toml")

            # act
            result = _load_configured_excludes(root)

            # assert
            self.assertIsNone(result)

    def test_should_return_none_when_exclude_key_is_not_a_list(self) -> None:
        """Test that a non-list exclude value is ignored (falls back to defaults) rather than crashing."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            (root / "pyproject.toml").write_text('[tool.typedframes]\nexclude = ".claude"\n')

            # act
            result = _load_configured_excludes(root)

            # assert
            self.assertIsNone(result)

    def test_should_ignore_non_string_entries_in_exclude_list(self) -> None:
        """Test that non-string entries in the exclude list are filtered out rather than crashing."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            (root / "pyproject.toml").write_text('[tool.typedframes]\nexclude = [".claude", 42]\n')

            # act
            result = _load_configured_excludes(root)

            # assert
            self.assertEqual(result, frozenset({".claude"}))

    def test_should_prune_configured_exclude_directory_end_to_end_via_main(self) -> None:
        """Test that a configured [tool.typedframes] exclude directory is pruned by a real `check` run."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            (root / "pyproject.toml").write_text('[tool.typedframes]\nexclude = [".claude"]\n')

            claude_dir = root / ".claude"
            claude_dir.mkdir()
            (claude_dir / "broken.py").write_text(
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
                main(["check", str(root), "--no-index"])

            # assert -- .claude/broken.py's unknown-column access would otherwise be a
            # real error; it never gets collected at all, so the run is clean
            output = captured.getvalue()
            self.assertIn("✓ Checked 0 files", output)

    def test_should_walk_venv_again_once_exclude_is_configured_end_to_end_via_main(self) -> None:
        """Test that configuring exclude without re-listing .venv lets .venv be walked again, via a real check run."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            (root / "pyproject.toml").write_text('[tool.typedframes]\nexclude = ["custom_dir"]\n')

            venv_dir = root / ".venv"
            venv_dir.mkdir()
            (venv_dir / "broken.py").write_text(
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
                main(["check", str(root), "--no-index"])

            # assert -- .venv/broken.py IS collected and its unknown-column error IS
            # reported, since exclude replaced the default set instead of adding to it
            output = captured.getvalue()
            self.assertIn("✗ Found 1 error", output)
            self.assertIn("wrong", output)

    def test_should_skip_file_that_raises_oserror_and_continue(self) -> None:
        """Test that a per-file OSError from check_file is skipped, not fatal.

        Covers e.g. non-UTF-8 content; the run should continue and report a stderr message.
        """
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            bad_file = Path(tmpdir) / "bad.py"
            bad_file.write_bytes(b"\xff\xfe# not utf8\n")

            good_file = Path(tmpdir) / "good.py"
            good_file.write_text(
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
            with patch("sys.stderr", captured):
                all_errors, _stats = _check_files([bad_file, good_file])

            # assert
            self.assertTrue(any(e["file"] == str(good_file) for e in all_errors))
            self.assertIn(f"{bad_file}: skipped,", captured.getvalue())

    def test_should_skip_file_with_invalid_syntax_and_continue(self) -> None:
        """Test that a per-file RuntimeError (parse failure) from check_file is skipped, not fatal.

        Covers e.g. unresolved git merge-conflict markers left in a .py file; the run
        should continue and report a stderr message rather than crash the whole scan.
        """
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            bad_file = Path(tmpdir) / "bad.py"
            bad_file.write_text("<<<<<<< Updated upstream\nimport foo\n=======\nimport bar\n>>>>>>> Stashed changes\n")

            good_file = Path(tmpdir) / "good.py"
            good_file.write_text(
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
            with patch("sys.stderr", captured):
                all_errors, _stats = _check_files([bad_file, good_file])

            # assert
            self.assertTrue(any(e["file"] == str(good_file) for e in all_errors))
            self.assertIn(f"{bad_file}: skipped,", captured.getvalue())

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

    def test_should_match_any_depth_beneath_a_double_star_glob(self) -> None:
        """Test that `**` spans any number of path segments."""
        # arrange
        pattern = _glob_to_regex("legacy/**")

        # act / assert
        self.assertTrue(pattern.match("legacy/old.py"))
        self.assertTrue(pattern.match("legacy/etl/deep/load.py"))
        self.assertFalse(pattern.match("src/legacy/old.py"))

    def test_should_not_let_single_star_cross_a_path_separator(self) -> None:
        """Test that `*` matches within one path segment only."""
        # arrange
        pattern = _glob_to_regex("src/*.py")

        # act / assert
        self.assertTrue(pattern.match("src/a.py"))
        self.assertFalse(pattern.match("src/nested/a.py"))

    def test_should_match_root_level_file_with_leading_double_star(self) -> None:
        """Test that `**/x.py` also matches `x.py` with no leading directory."""
        # arrange
        pattern = _glob_to_regex("**/conftest.py")

        # act / assert
        self.assertTrue(pattern.match("conftest.py"))
        self.assertTrue(pattern.match("tests/unit/conftest.py"))

    def test_should_prefer_the_most_specific_override_pattern(self) -> None:
        """Test that the longest literal prefix wins when several globs match."""
        # arrange
        config = CoverageConfig(enabled=True, overrides=(("src/**", 50.0), ("src/new_module/**", 100.0)))

        # act
        match = _override_for("src/new_module/loader.py", config)

        # assert
        self.assertEqual(("src/new_module/**", 100.0), match)

    def test_should_return_no_override_when_no_pattern_matches(self) -> None:
        """Test that an unmatched file falls through to the global threshold."""
        # arrange
        config = CoverageConfig(enabled=True, overrides=(("legacy/**", 50.0),))

        # act
        match = _override_for("src/loader.py", config)

        # assert
        self.assertIsNone(match)

    def test_should_report_failing_bucket_when_below_global_threshold(self) -> None:
        """Test that coverage under the global fail_under produces a failing bucket."""
        # arrange
        per_file = {"/proj/a.py": (4, 1)}
        config = CoverageConfig(enabled=True, fail_under=90.0)

        # act
        failing = _evaluate_coverage(per_file, config, Path("/proj"), None)

        # assert
        self.assertEqual(1, len(failing))
        self.assertIsNone(failing[0].label)
        self.assertEqual(25.0, failing[0].pct)

    def test_should_report_no_failures_when_threshold_is_met(self) -> None:
        """Test that coverage at or above the threshold passes."""
        # arrange
        per_file = {"/proj/a.py": (4, 4)}
        config = CoverageConfig(enabled=True, fail_under=100.0)

        # act
        failing = _evaluate_coverage(per_file, config, Path("/proj"), None)

        # assert
        self.assertEqual([], failing)

    def test_should_grade_override_bucket_separately_from_global_bucket(self) -> None:
        """Test that a per-path override is judged on its own files, not the whole project."""
        # arrange
        per_file = {"/proj/legacy/old.py": (4, 0), "/proj/src/new.py": (2, 2)}
        config = CoverageConfig(enabled=True, fail_under=100.0, overrides=(("legacy/**", 0.0),))

        # act
        failing = _evaluate_coverage(per_file, config, Path("/proj"), None)

        # assert
        self.assertEqual([], failing)

    def test_should_fail_only_the_override_bucket_that_misses_its_own_bar(self) -> None:
        """Test that a failing override is named in the report while the global bucket passes."""
        # arrange
        per_file = {"/proj/legacy/old.py": (4, 1), "/proj/src/new.py": (2, 2)}
        config = CoverageConfig(enabled=True, fail_under=100.0, overrides=(("legacy/**", 50.0),))

        # act
        failing = _evaluate_coverage(per_file, config, Path("/proj"), None)

        # assert
        self.assertEqual(1, len(failing))
        self.assertEqual("legacy/**", failing[0].label)

    def test_should_pass_vacuously_when_a_bucket_has_no_dataframes(self) -> None:
        """Test that 0/0 is treated as nothing to measure rather than a failure."""
        # arrange
        per_file = {"/proj/a.py": (0, 0)}
        config = CoverageConfig(enabled=True, fail_under=100.0)

        # act
        failing = _evaluate_coverage(per_file, config, Path("/proj"), None)

        # assert
        self.assertEqual([], failing)

    def test_should_ignore_path_overrides_when_fail_under_flag_is_given(self) -> None:
        """Test that --coverage-fail-under is a total override, not merged with config overrides."""
        # arrange
        per_file = {"/proj/legacy/old.py": (4, 0)}
        config = CoverageConfig(enabled=True, fail_under=100.0, overrides=(("legacy/**", 0.0),))

        # act
        failing = _evaluate_coverage(per_file, config, Path("/proj"), 100.0)

        # assert
        self.assertEqual(1, len(failing))
        self.assertIsNone(failing[0].label)

    def test_should_return_disabled_coverage_config_when_nothing_configured(self) -> None:
        """Test that a project with no config table gets no threshold."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            # act
            config = _load_coverage_config(Path(tmpdir))

            # assert
            self.assertFalse(config.enabled)
            self.assertEqual((), config.overrides)

    def test_should_load_coverage_config_from_pyproject_toml(self) -> None:
        """Test that [tool.typedframes.coverage] is read from pyproject.toml."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "pyproject.toml").write_text(
                "[tool.typedframes.coverage]\nenabled = true\nfail_under = 80.0\n\n"
                '[tool.typedframes.coverage.overrides]\n"legacy/**" = 25.0\n'
            )

            # act
            config = _load_coverage_config(Path(tmpdir))

            # assert
            self.assertTrue(config.enabled)
            self.assertEqual(80.0, config.fail_under)
            self.assertEqual((("legacy/**", 25.0),), config.overrides)

    def test_should_prefer_standalone_typedframes_toml_over_pyproject(self) -> None:
        """Test that typedframes.toml wins entirely rather than merging with pyproject.toml."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "pyproject.toml").write_text(
                "[tool.typedframes.coverage]\nenabled = true\nfail_under = 99.0\n"
            )
            (Path(tmpdir) / "typedframes.toml").write_text("[coverage]\nenabled = true\nfail_under = 10.0\n")

            # act
            config = _load_coverage_config(Path(tmpdir))

            # assert
            self.assertEqual(10.0, config.fail_under)

    def test_should_disable_coverage_when_config_toml_is_malformed(self) -> None:
        """Test that unparseable config warns and leaves enforcement off."""
        # arrange
        captured = StringIO()
        with tempfile.TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "typedframes.toml").write_text("[coverage\nenabled = true\n")

            # act
            with patch("sys.stderr", captured):
                config = _load_coverage_config(Path(tmpdir))

            # assert
            self.assertFalse(config.enabled)
            self.assertIn("typedframes.toml", captured.getvalue())

    def test_should_warn_and_fall_back_when_fail_under_is_not_a_number(self) -> None:
        """Test that a non-numeric fail_under is reported rather than silently accepted."""
        # arrange
        captured = StringIO()
        with tempfile.TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "typedframes.toml").write_text('[coverage]\nenabled = true\nfail_under = "ninety"\n')

            # act
            with patch("sys.stderr", captured):
                config = _load_coverage_config(Path(tmpdir))

            # assert
            self.assertEqual(100.0, config.fail_under)
            self.assertIn("coverage.fail_under", captured.getvalue())

    def test_should_warn_and_fall_back_when_fail_under_is_out_of_range(self) -> None:
        """Test that a percentage outside 0-100 is rejected."""
        # arrange
        captured = StringIO()
        with tempfile.TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "typedframes.toml").write_text("[coverage]\nenabled = true\nfail_under = 150.0\n")

            # act
            with patch("sys.stderr", captured):
                config = _load_coverage_config(Path(tmpdir))

            # assert
            self.assertEqual(100.0, config.fail_under)
            self.assertIn("0-100", captured.getvalue())

    def test_should_reject_a_boolean_fail_under(self) -> None:
        """Test that `fail_under = true` is a mistake, not 100%."""
        # arrange
        captured = StringIO()
        with tempfile.TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "typedframes.toml").write_text("[coverage]\nenabled = true\nfail_under = true\n")

            # act
            with patch("sys.stderr", captured):
                config = _load_coverage_config(Path(tmpdir))

            # assert
            self.assertIn("expected a number", captured.getvalue())
            self.assertEqual(100.0, config.fail_under)

    def test_should_produce_identical_output_when_coverage_table_is_absent(self) -> None:
        """Test the opt-in guarantee: no coverage table means behaviour is unchanged."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "load.py").write_text('import pandas as pd\ndf = pd.read_csv("a.csv")\n')

            without_config = StringIO()
            with patch("sys.stdout", without_config):
                main(["check", tmpdir])

            (Path(tmpdir) / "pyproject.toml").write_text("[tool.typedframes]\nenabled = true\n")

            # act
            with_config = StringIO()
            with patch("sys.stdout", with_config):
                main(["check", tmpdir])

            # assert
            self.assertEqual(without_config.getvalue(), with_config.getvalue())
            self.assertNotIn("below the required", without_config.getvalue())

    def test_should_not_enforce_threshold_when_coverage_is_disabled(self) -> None:
        """Test that a present-but-disabled table leaves the run passing."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "load.py").write_text('import pandas as pd\ndf = pd.read_csv("a.csv")\n')
            (Path(tmpdir) / "typedframes.toml").write_text("[coverage]\nenabled = false\nfail_under = 100.0\n")

            captured = StringIO()

            # act
            with patch("sys.stdout", captured):
                main(["check", tmpdir])

            # assert
            self.assertNotIn("below the required", captured.getvalue())

    def test_should_exit_1_when_coverage_is_below_configured_threshold(self) -> None:
        """Test that an enabled threshold fails the run end to end."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "load.py").write_text('import pandas as pd\ndf = pd.read_csv("a.csv")\n')
            (Path(tmpdir) / "typedframes.toml").write_text("[coverage]\nenabled = true\nfail_under = 100.0\n")

            captured = StringIO()

            # act
            with patch("sys.stdout", captured), self.assertRaises(SystemExit) as ctx:
                main(["check", tmpdir])

            # assert
            self.assertEqual(1, ctx.exception.code)
            self.assertIn("below the required 100.0%", captured.getvalue())

    def test_should_exit_1_when_fail_under_flag_is_not_met_without_any_config(self) -> None:
        """Test that --coverage-fail-under enforces a threshold on an otherwise unconfigured project."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "load.py").write_text('import pandas as pd\ndf = pd.read_csv("a.csv")\n')

            captured = StringIO()

            # act
            with patch("sys.stdout", captured), self.assertRaises(SystemExit) as ctx:
                main(["check", tmpdir, "--coverage-fail-under", "50"])

            # assert
            self.assertEqual(1, ctx.exception.code)
            self.assertIn("below the required 50.0%", captured.getvalue())

    def test_should_exit_2_when_fail_under_flag_is_out_of_range(self) -> None:
        """Test that an invalid --coverage-fail-under is a usage error, not a coverage failure."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "load.py").write_text("x = 1\n")

            # act / assert
            with patch("sys.stderr", StringIO()), self.assertRaises(SystemExit) as ctx:
                main(["check", tmpdir, "--coverage-fail-under", "150"])

            self.assertEqual(2, ctx.exception.code)

    def test_should_report_coverage_failure_without_corrupting_json_output(self) -> None:
        """Test that the gate message goes to stderr so stdout stays valid JSON."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "load.py").write_text('import pandas as pd\ndf = pd.read_csv("a.csv")\n')
            (Path(tmpdir) / "typedframes.toml").write_text("[coverage]\nenabled = true\nfail_under = 100.0\n")

            out, err = StringIO(), StringIO()

            # act
            with patch("sys.stdout", out), patch("sys.stderr", err), self.assertRaises(SystemExit):
                main(["check", tmpdir, "--output-format", "json"])

            # assert
            json.loads(out.getvalue())
            self.assertIn("below the required", err.getvalue())

    def test_should_still_report_coverage_failure_when_info_is_suppressed(self) -> None:
        """Test that --no-info hides the informational line but not a failed gate."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "load.py").write_text('import pandas as pd\ndf = pd.read_csv("a.csv")\n')
            (Path(tmpdir) / "typedframes.toml").write_text("[coverage]\nenabled = true\nfail_under = 100.0\n")

            captured = StringIO()

            # act
            with patch("sys.stdout", captured), self.assertRaises(SystemExit):
                main(["check", tmpdir, "--no-info"])

            # assert
            output = captured.getvalue()
            self.assertNotIn("DataFrame schema coverage, not a pass/fail result", output)
            self.assertIn("below the required", output)

    def test_should_ignore_config_when_an_intermediate_key_is_not_a_table(self) -> None:
        """Test that `tool.typedframes` holding a scalar can't crash config loading."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "pyproject.toml").write_text('[tool]\ntypedframes = "yes"\n')

            # act
            config = _load_coverage_config(Path(tmpdir))

            # assert
            self.assertFalse(config.enabled)

    def test_should_warn_and_disable_when_enabled_is_not_a_boolean(self) -> None:
        """Test that a non-boolean `enabled` is rejected rather than coerced."""
        # arrange
        captured = StringIO()
        with tempfile.TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "typedframes.toml").write_text('[coverage]\nenabled = "true"\n')

            # act
            with patch("sys.stderr", captured):
                config = _load_coverage_config(Path(tmpdir))

            # assert
            self.assertFalse(config.enabled)
            self.assertIn("coverage.enabled", captured.getvalue())

    def test_should_keep_default_fail_under_when_key_is_omitted(self) -> None:
        """Test that enabling coverage without naming a threshold uses the default."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "typedframes.toml").write_text("[coverage]\nenabled = true\n")

            # act
            config = _load_coverage_config(Path(tmpdir))

            # assert
            self.assertTrue(config.enabled)
            self.assertEqual(100.0, config.fail_under)

    def test_should_drop_only_the_invalid_entry_from_the_overrides_table(self) -> None:
        """Test that one bad override doesn't discard the valid ones beside it."""
        # arrange
        captured = StringIO()
        with tempfile.TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "typedframes.toml").write_text(
                '[coverage]\nenabled = true\n\n[coverage.overrides]\n"legacy/**" = "half"\n"src/**" = 75.0\n'
            )

            # act
            with patch("sys.stderr", captured):
                config = _load_coverage_config(Path(tmpdir))

            # assert
            self.assertEqual((("src/**", 75.0),), config.overrides)
            self.assertIn("legacy/**", captured.getvalue())

    def test_should_warn_when_overrides_is_not_a_table(self) -> None:
        """Test that `overrides` given as a scalar is reported and ignored."""
        # arrange
        captured = StringIO()
        with tempfile.TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "typedframes.toml").write_text('[coverage]\nenabled = true\noverrides = "legacy"\n')

            # act
            with patch("sys.stderr", captured):
                config = _load_coverage_config(Path(tmpdir))

            # assert
            self.assertEqual((), config.overrides)
            self.assertIn("coverage.overrides", captured.getvalue())

    def test_should_match_exactly_one_character_for_a_question_mark_glob(self) -> None:
        """Test that `?` matches a single character within one path segment."""
        # arrange
        pattern = _glob_to_regex("src/v?/load.py")

        # act / assert
        self.assertTrue(pattern.match("src/v1/load.py"))
        self.assertFalse(pattern.match("src/v10/load.py"))

    def test_should_treat_an_empty_bucket_as_fully_covered(self) -> None:
        """Test that a bucket with no DataFrames reports 100% rather than dividing by zero."""
        # arrange
        bucket = CoverageBucket(label=None, threshold=100.0, total=0, typed=0)

        # act
        pct = bucket.pct

        # assert
        self.assertEqual(100.0, pct)

    def test_should_fall_back_to_absolute_path_when_file_is_outside_the_root(self) -> None:
        """Test that a file that isn't under the checked root still yields a matchable path."""
        # arrange
        root = Path("/proj")

        # act
        rel = _relative_posix("/elsewhere/mod.py", root)

        # assert
        self.assertEqual("/elsewhere/mod.py", rel)

    def test_should_reject_a_non_numeric_fail_under_flag(self) -> None:
        """Test that --coverage-fail-under=abc is a usage error rather than a crash."""
        # act / assert
        with self.assertRaises(argparse.ArgumentTypeError):
            _percentage("abc")

    def test_should_emit_github_error_annotation_for_a_failed_threshold(self) -> None:
        """Test that github output format reports the gate as a workflow annotation."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "load.py").write_text('import pandas as pd\ndf = pd.read_csv("a.csv")\n')
            (Path(tmpdir) / "typedframes.toml").write_text("[coverage]\nenabled = true\nfail_under = 100.0\n")

            captured = StringIO()

            # act
            with patch("sys.stdout", captured), self.assertRaises(SystemExit):
                main(["check", tmpdir, "--output-format", "github"])

            # assert
            self.assertIn(
                "::error title=typedframes DataFrame schema coverage::DataFrame schema coverage",
                captured.getvalue(),
            )

    def test_should_exit_0_when_enabled_threshold_is_satisfied(self) -> None:
        """Test that an enabled but satisfied threshold leaves the run passing."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "load.py").write_text('import pandas as pd\ndf = pd.read_csv("a.csv", usecols=["a"])\n')
            (Path(tmpdir) / "typedframes.toml").write_text("[coverage]\nenabled = true\nfail_under = 100.0\n")

            captured = StringIO()

            # act
            with patch("sys.stdout", captured):
                main(["check", tmpdir])

            # assert
            self.assertNotIn("below the required", captured.getvalue())

    def test_should_render_per_file_table_with_missing_sites(self) -> None:
        """Test that term-missing lists each file's tally and the sites lacking column info."""
        # arrange
        per_file = {"/proj/src/new.py": (2, 1), "/proj/legacy/old.py": (2, 0)}
        sites = [
            {"file": "/proj/legacy/old.py", "line": 2, "col": 1, "var": "old_one"},
            {"file": "/proj/src/new.py", "line": 3, "col": 1, "var": "bad"},
        ]

        # act
        table = _format_term_missing(per_file, sites, Path("/proj"))

        # assert
        self.assertIn("legacy/old.py", table)
        self.assertIn("old_one:2", table)
        self.assertIn("bad:3", table)
        self.assertIn("TOTAL", table)

    def test_should_omit_files_with_no_dataframes_from_the_table(self) -> None:
        """Test that a 0/0 file is left out rather than padding the report."""
        # arrange
        per_file = {"/proj/empty.py": (0, 0), "/proj/real.py": (1, 1)}

        # act
        table = _format_term_missing(per_file, [], Path("/proj"))

        # assert
        self.assertNotIn("empty.py", table)
        self.assertIn("real.py", table)

    def test_should_report_nothing_to_measure_when_no_file_has_dataframes(self) -> None:
        """Test that an entirely DataFrame-free run says so rather than printing an empty table."""
        # arrange
        per_file = {"/proj/a.py": (0, 0)}

        # act
        table = _format_term_missing(per_file, [], Path("/proj"))

        # assert
        self.assertEqual("No DataFrames with recognized loads/schemas found to check", table)

    def test_should_report_ratio_once_any_dataframe_is_counted(self) -> None:
        """Test the exact wording once dataframes_total>0.

        The detection fixes in the Rust checker (control-flow bodies, class-method
        bodies, and Protocol/structural-typing call sites all being visited) turn many
        former "0 found" runs into a real, non-zero denominator; this locks in that
        once that happens, the message reports an honest ratio rather than staying
        silent.
        """
        # arrange
        stats = RunStats(elapsed=0.1, dataframes_total=15, dataframes_typed=0)

        # act
        message = _coverage_message(stats)

        # assert
        self.assertIn("0/15 DataFrames had column info", message)
        self.assertIn("(0%)", message)

    def test_should_report_no_dataframes_found_only_when_denominator_is_zero(self) -> None:
        """Test that the "nothing found" message requires a genuinely empty denominator.

        Not conflated with "found some, none had derivable schemas".
        """
        # arrange
        stats = RunStats(elapsed=0.1, dataframes_total=0, dataframes_typed=0)

        # act
        message = _coverage_message(stats)

        # assert
        self.assertEqual("\u2139 No DataFrames with recognized loads/schemas found to check", message)

    def test_should_build_json_coverage_payload_with_unrounded_percentages(self) -> None:
        """Test that the JSON report keeps the exact ratio for machine consumers."""
        # arrange
        per_file = {"/proj/a.py": (3, 1)}
        sites = [
            {"file": "/proj/a.py", "line": 5, "col": 1, "var": "x"},
            {"file": "/proj/a.py", "line": 4, "col": 1, "var": "y"},
        ]

        # act
        payload = _coverage_json_payload(per_file, sites, Path("/proj"))

        # assert
        self.assertAlmostEqual(100 / 3, payload["percent"])
        self.assertEqual("a.py", payload["files"][0]["file"])
        self.assertEqual([4, 5], [m["line"] for m in payload["files"][0]["missing"]])

    def test_should_use_null_percent_when_a_file_has_no_dataframes(self) -> None:
        """Test that the JSON report reports null rather than dividing by zero."""
        # arrange
        per_file = {"/proj/a.py": (0, 0)}

        # act
        payload = _coverage_json_payload(per_file, [], Path("/proj"))

        # assert
        self.assertIsNone(payload["percent"])
        self.assertIsNone(payload["files"][0]["percent"])

    def test_should_attach_untyped_sites_to_their_source_file(self) -> None:
        """Test that _check_files tags each untyped site with the file it came from."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            py_file = Path(tmpdir) / "load.py"
            py_file.write_text('import pandas as pd\nsales = pd.read_csv("a.csv")\n')

            # act
            _errors, stats = _check_files([py_file])

            # assert
            self.assertEqual(1, len(stats["untyped_sites"]))
            self.assertEqual(str(py_file), stats["untyped_sites"][0]["file"])
            self.assertEqual("sales", stats["untyped_sites"][0]["var"])

    def test_should_keep_untyped_site_count_matching_the_coverage_shortfall(self) -> None:
        """Test the invariant term-missing relies on: sites == total - typed."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            py_file = Path(tmpdir) / "load.py"
            py_file.write_text(
                "import pandas as pd\n"
                'good = pd.read_csv("a.csv", usecols=["a"])\n'
                'bad_one = pd.read_csv("b.csv")\n'
                'bad_two = pd.read_csv("c.csv")\n'
            )

            # act
            _errors, stats = _check_files([py_file])

            # assert
            shortfall = stats["dataframes_total"] - stats["dataframes_typed"]
            self.assertEqual(shortfall, len(stats["untyped_sites"]))

    def test_should_print_term_missing_table_end_to_end(self) -> None:
        """Test that --coverage-detail=term-missing prints the per-file breakdown."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "load.py").write_text('import pandas as pd\nsales = pd.read_csv("a.csv")\n')

            captured = StringIO()

            # act
            with patch("sys.stdout", captured):
                main(["check", tmpdir, "--no-warnings", "--coverage-detail", "term-missing"])

            # assert
            output = captured.getvalue()
            self.assertIn("Missing", output)
            self.assertIn("sales:2", output)

    def test_should_reject_json_as_a_coverage_detail_value(self) -> None:
        """Test that json is not a --coverage-detail value -- --output-format owns format."""
        # arrange / act / assert
        with self.assertRaises(SystemExit):
            main(["check", ".", "--coverage-detail", "json"])

    def test_should_nest_coverage_detail_inside_json_output_format(self) -> None:
        """Test that --coverage-detail=term-missing nests as structured JSON under --output-format=json."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "load.py").write_text('import pandas as pd\nsales = pd.read_csv("a.csv")\n')

            captured = StringIO()

            # act
            with patch("sys.stdout", captured):
                main(["check", tmpdir, "--output-format", "json", "--coverage-detail", "term-missing"])

            # assert
            payload = json.loads(captured.getvalue())
            self.assertIn("coverage", payload)
            self.assertEqual("sales", payload["coverage"]["files"][0]["missing"][0]["var"])

    def test_should_omit_coverage_key_from_json_output_by_default(self) -> None:
        """Test that the default JSON payload is unchanged by this feature."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "load.py").write_text('import pandas as pd\nsales = pd.read_csv("a.csv")\n')

            captured = StringIO()

            # act
            with patch("sys.stdout", captured):
                main(["check", tmpdir, "--output-format", "json"])

            # assert
            payload = json.loads(captured.getvalue())
            self.assertEqual(["errors", "stats"], sorted(payload))

    def test_should_read_detail_level_from_config(self) -> None:
        """Test that the `detail` key selects a detail level without a CLI flag."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "typedframes.toml").write_text('[coverage]\ndetail = "term-missing"\n')

            # act
            config = _load_coverage_config(Path(tmpdir))

            # assert
            self.assertEqual("term-missing", config.detail)

    def test_should_warn_and_fall_back_when_detail_level_is_unknown(self) -> None:
        """Test that an unrecognized detail level is reported rather than silently accepted."""
        # arrange
        captured = StringIO()
        with tempfile.TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "typedframes.toml").write_text('[coverage]\ndetail = "html"\n')

            # act
            with patch("sys.stderr", captured):
                config = _load_coverage_config(Path(tmpdir))

            # assert
            self.assertEqual("summary", config.detail)
            self.assertIn("coverage.detail", captured.getvalue())

    def test_should_let_the_flag_override_the_configured_detail_level(self) -> None:
        """Test that --coverage-detail wins over the `detail` config key."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "load.py").write_text('import pandas as pd\nsales = pd.read_csv("a.csv")\n')
            (Path(tmpdir) / "typedframes.toml").write_text('[coverage]\ndetail = "term-missing"\n')

            captured = StringIO()

            # act
            with patch("sys.stdout", captured):
                main(["check", tmpdir, "--no-warnings", "--coverage-detail", "summary"])

            # assert
            self.assertNotIn("Missing", captured.getvalue())

    def test_should_report_detail_without_enabling_enforcement(self) -> None:
        """Test that `detail` is independent of `enabled`: detail without a gate."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "load.py").write_text('import pandas as pd\nsales = pd.read_csv("a.csv")\n')
            (Path(tmpdir) / "typedframes.toml").write_text('[coverage]\nenabled = false\ndetail = "term-missing"\n')

            captured = StringIO()

            # act
            with patch("sys.stdout", captured):
                main(["check", tmpdir, "--no-warnings"])

            # assert
            output = captured.getvalue()
            self.assertIn("sales:2", output)
            self.assertNotIn("below the required", output)

    def test_should_collect_single_notebook_file(self) -> None:
        """Test collecting a single .ipynb file."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            nb_file = Path(tmpdir) / "test.ipynb"
            _write_notebook(nb_file, ["x = 1"])

            # act
            result = _collect_notebook_files(nb_file)

            # assert
            self.assertEqual(result, [nb_file])

    def test_should_skip_non_notebook_file_when_collecting_notebooks(self) -> None:
        """Test that _collect_notebook_files skips non-.ipynb files."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            py_file = Path(tmpdir) / "test.py"
            py_file.write_text("x = 1")

            # act
            result = _collect_notebook_files(py_file)

            # assert
            self.assertEqual(result, [])

    def test_should_collect_notebook_files_from_directory(self) -> None:
        """Test recursive collection of .ipynb files from a directory."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            _write_notebook(root / "a.ipynb", ["x = 1"])
            (root / "b.py").write_text("y = 2")
            sub = root / "sub"
            sub.mkdir()
            _write_notebook(sub / "c.ipynb", ["z = 3"])

            # act
            result = _collect_notebook_files(root)

            # assert
            names = [f.name for f in result]
            self.assertEqual(sorted(names), ["a.ipynb", "c.ipynb"])

    def test_should_report_notebook_column_error_with_cell_relative_location(self) -> None:
        """Test that a notebook error is reported against the notebook path with cell-relative line/col."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            nb_file = Path(tmpdir) / "analysis.ipynb"
            _write_notebook(
                nb_file,
                [
                    "from typedframes import BaseSchema, Column\n\nclass S(BaseSchema):\n    x = Column(type=int)",
                    'df: "DataFrame[S]" = load()\ndf["wrong"]',
                ],
            )

            # act
            all_errors, _stats = _check_files([nb_file])

            # assert
            self.assertEqual(1, len(all_errors))
            error = all_errors[0]
            self.assertEqual(str(nb_file), error["file"])
            self.assertEqual(2, error["cell"])
            self.assertEqual(2, error["line"])

    def test_should_point_defined_at_reference_at_the_notebook_not_a_synthetic_file(self) -> None:
        """Test that a same-notebook schema's "(defined at ...)" reference names the notebook.

        The Rust `check_notebook` entry point checks the notebook's concatenated code-cell
        source directly (via `ruff_notebook`) and translates results back to cell/line itself
        -- this is a regression test for that translation, not for any Python-side rewriting.
        """
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            nb_file = Path(tmpdir) / "analysis.ipynb"
            _write_notebook(
                nb_file,
                [
                    "from typedframes import BaseSchema, Column\n\nclass S(BaseSchema):\n    x = Column(type=int)",
                    'df: "DataFrame[S]" = load()\ndf["wrong"]',
                ],
            )

            # act
            all_errors, _stats = _check_files([nb_file])

            # assert
            message = all_errors[0]["message"]
            self.assertIn(f"defined at {nb_file} cell 1:3", message)
            self.assertNotIn(".py", message)

    def test_should_remap_every_error_when_a_notebook_has_more_than_one(self) -> None:
        """Test that cell/line remapping applies to every error, not just the first."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            nb_file = Path(tmpdir) / "two_bugs.ipynb"
            _write_notebook(
                nb_file,
                [
                    "from typedframes import BaseSchema, Column\n\nclass S(BaseSchema):\n    x = Column(type=int)",
                    'df: "DataFrame[S]" = load()\ndf["wrong"]\ndf["also_wrong"]',
                ],
            )

            # act
            all_errors, _stats = _check_files([nb_file])

            # assert
            self.assertEqual(2, len(all_errors))
            self.assertEqual([(2, 2), (2, 3)], sorted((e["cell"], e["line"]) for e in all_errors))

    def test_should_attach_cell_to_untyped_dataframe_sites_too(self) -> None:
        """Test that untyped-DataFrame coverage sites, not just errors, also carry a `cell`.

        A bare `pd.read_csv(...)` produces both an untracked-dataframe warning (an error with
        a `message`) and a plain coverage site entry (no `message`) -- both must come back
        cell-tagged.
        """
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            nb_file = Path(tmpdir) / "untyped.ipynb"
            _write_notebook(nb_file, ["import pandas as pd\ndf = pd.read_csv('x.csv')"])

            # act
            all_errors, stats = _check_files([nb_file], index_bytes=None)

            # assert
            self.assertEqual(1, len(all_errors))
            self.assertEqual(1, all_errors[0]["cell"])
            untyped_sites = stats["untyped_sites"]
            self.assertEqual(1, len(untyped_sites))
            self.assertEqual(1, untyped_sites[0]["cell"])
            self.assertNotIn("message", untyped_sites[0])

    def test_should_not_report_dataframes_from_a_clean_notebook(self) -> None:
        """Test that a notebook with no column errors produces none."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            nb_file = Path(tmpdir) / "clean.ipynb"
            _write_notebook(nb_file, ["x = 1"])

            # act
            all_errors, _stats = _check_files([nb_file])

            # assert
            self.assertEqual([], all_errors)

    def test_should_tolerate_ipython_magic_in_notebook_cell(self) -> None:
        """Test that a line magic doesn't stop the rest of the cell from being checked."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            nb_file = Path(tmpdir) / "magic.ipynb"
            _write_notebook(
                nb_file,
                [
                    "from typedframes import BaseSchema, Column\n\nclass S(BaseSchema):\n    x = Column(type=int)",
                    '%matplotlib inline\ndf: "DataFrame[S]" = load()\ndf["wrong"]',
                ],
            )

            # act
            all_errors, _stats = _check_files([nb_file])

            # assert
            self.assertEqual(1, len(all_errors))
            self.assertEqual(3, all_errors[0]["line"])

    def test_should_skip_malformed_notebook_and_continue(self) -> None:
        """Test that a notebook that isn't valid notebook JSON is skipped, not fatal."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            bad_nb = Path(tmpdir) / "bad.ipynb"
            bad_nb.write_text("{not valid json")

            good_file = Path(tmpdir) / "good.py"
            good_file.write_text(
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
            with patch("sys.stderr", captured):
                all_errors, _stats = _check_files([bad_nb, good_file])

            # assert
            self.assertTrue(any(e["file"] == str(good_file) for e in all_errors))
            self.assertIn(f"{bad_nb}: skipped,", captured.getvalue())

    def test_should_exit_1_when_strict_and_notebook_has_errors(self) -> None:
        """Test the full main() path for a single .ipynb given directly on the command line."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            nb_file = Path(tmpdir) / "bad.ipynb"
            _write_notebook(
                nb_file,
                [
                    "from typedframes import BaseSchema, Column\n\nclass S(BaseSchema):\n    x = Column(type=int)",
                    'df: "DataFrame[S]" = load()\ndf["wrong"]',
                ],
            )

            # act / assert
            with self.assertRaises(SystemExit) as ctx:
                main(["check", str(nb_file), "--strict"])
            self.assertEqual(ctx.exception.code, 1)

    def test_should_check_directory_containing_both_python_and_notebook_files(self) -> None:
        """Test that `check` on a directory picks up both .py and .ipynb files."""
        # arrange
        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            (root / "clean.py").write_text("x = 1\n")
            _write_notebook(root / "clean.ipynb", ["y = 2"])

            captured = StringIO()

            # act
            with patch("sys.stdout", captured):
                main(["check", str(root)])

            # assert
            self.assertIn("✓ Checked 2 files", captured.getvalue())

    def test_should_format_text_with_cell_location_for_notebook_errors(self) -> None:
        """Test that text formatting renders `cell N:line:col` for notebook errors."""
        # arrange
        errors = [
            {
                "file": "analysis.ipynb",
                "cell": 3,
                "line": 2,
                "col": 5,
                "severity": "error",
                "code": "unknown-column",
                "message": "Column 'wrong' not in S",
            }
        ]

        # act
        result = _format_text(errors)

        # assert
        self.assertEqual(
            "analysis.ipynb:cell 3:2:5: error[unknown-column] Column 'wrong' not in S",
            result,
        )

    def test_should_format_github_annotation_title_with_notebook_cell(self) -> None:
        """Test that a notebook error's cell number is folded into the GitHub annotation title."""
        # arrange
        errors = [
            {
                "file": "analysis.ipynb",
                "cell": 2,
                "line": 2,
                "col": 1,
                "code": "unknown-column",
                "message": "Column 'wrong' not in S",
                "severity": "error",
            }
        ]

        # act
        result = _format_github(errors)

        # assert
        self.assertIn("title=unknown-column (notebook cell 2)", result)

    def test_should_skip_notebook_on_runtime_error_from_check_notebook(self) -> None:
        """Test that a RuntimeError from check_notebook is a skip, not fatal.

        Covers a genuine syntax error, malformed notebook JSON, or a non-Python kernel --
        see check_notebook in rust/src/pyapi.rs. Uses dependency injection (a fake
        `check_notebook`) rather than patching the real Rust function.
        """

        # arrange
        def _failing_check_notebook(_path: str, _index_bytes: bytes | None) -> str:
            msg = "boom"
            raise RuntimeError(msg)

        with tempfile.TemporaryDirectory() as tmpdir:
            nb_file = Path(tmpdir) / "nb.ipynb"
            _write_notebook(nb_file, ["x = 1"])

            captured = StringIO()

            # act
            with patch("sys.stderr", captured):
                result = _check_notebook_file(nb_file, _failing_check_notebook, None)

            # assert
            self.assertIsNone(result)
            self.assertIn(f"{nb_file}: skipped, boom", captured.getvalue())

    def test_should_skip_notebook_on_os_error_from_check_notebook(self) -> None:
        """Test that an OSError from check_notebook (e.g. the file can't be read) is a skip too."""

        # arrange
        def _failing_check_notebook(_path: str, _index_bytes: bytes | None) -> str:
            msg = "permission denied"
            raise OSError(msg)

        with tempfile.TemporaryDirectory() as tmpdir:
            nb_file = Path(tmpdir) / "nb.ipynb"
            _write_notebook(nb_file, ["x = 1"])

            captured = StringIO()

            # act
            with patch("sys.stderr", captured):
                result = _check_notebook_file(nb_file, _failing_check_notebook, None)

            # assert
            self.assertIsNone(result)
            self.assertIn(f"{nb_file}: skipped, permission denied", captured.getvalue())

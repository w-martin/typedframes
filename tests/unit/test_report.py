"""Tests for `typedframes report`."""

from io import StringIO
from json import dumps
from pathlib import Path
from re import MULTILINE, findall
from sys import modules
from tempfile import TemporaryDirectory
from types import SimpleNamespace
from unittest import TestCase
from unittest.mock import patch

from typedframes.cli import _format_report, main

_PRIVATE = (
    "import pandas as pd\n"
    "from acme_internal import secret_loader\n\n\n"
    "def run(customer_id):\n"
    '    orders = pd.read_csv("s3://acme/orders.csv", sep=";")\n'
    '    total = orders["gross_margin"].sum()\n'
    "    return clean_the_data(orders), total, customer_id\n"
)


def _run(argv: list[str]) -> tuple[str, str, int | None]:
    """Run the CLI, returning stdout, stderr and the exit code (None for a normal return)."""
    out, err = StringIO(), StringIO()
    code: int | None = None
    with patch("sys.stdout", out), patch("sys.stderr", err):
        try:
            main(argv)
        except SystemExit as exit_:
            code = exit_.code if isinstance(exit_.code, int) else 1
    return out.getvalue(), err.getvalue(), code


def _name_of(root: Path) -> str:
    """The first site name `suggest` prints for the project at `root`."""
    listing, _err, _code = _run(["suggest", str(root), "--no-index"])
    return findall(r"^(?:  )?([a-z]+-[a-z]+)  ", listing, flags=MULTILINE)[0]


class TestReport(TestCase):
    """The bug-report body for one untyped site."""

    def test_should_print_the_verdict_and_a_snippet_without_the_original_names(self) -> None:
        """Test that the body has the checker's verdict and a redacted copy of the code."""
        # arrange
        with TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            (root / "pyproject.toml").write_text("")
            (root / "job.py").write_text(_PRIVATE)
            name = _name_of(root)

            # act
            out, err, code = _run(["report", name, str(root), "--no-index"])

            # assert
            self.assertIsNone(code)
            self.assertIn(f"## typedframes report: {name}", out)
            self.assertIn("- Outcome: withheld", out)
            self.assertIn("```python", out)
            self.assertIn("pd.read_csv(", out)
            for secret in ["acme", "customer", "orders", "gross_margin", "clean_the_data", "secret_loader"]:
                self.assertNotIn(secret, out)
            self.assertIn("Read the report before posting it", err)

    def test_should_exit_2_for_a_name_that_matches_no_site(self) -> None:
        """Test that an unknown name is a usage error."""
        # arrange
        with TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "job.py").write_text(_PRIVATE)

            # act
            out, err, code = _run(["report", "no-such", tmpdir, "--no-index"])

            # assert
            self.assertEqual(2, code)
            self.assertEqual("", out)
            self.assertIn("no untyped site is named 'no-such'", err)

    def test_should_list_the_candidates_when_a_name_matches_several_sites(self) -> None:
        """Test the error for a name two files share, which a hash collision can still cause."""

        # arrange
        def found(files: list[str], name: str, _index: bytes | None = None) -> list[tuple[str | None, str | None]]:
            return [(dumps({"name": name, "line": 6, "col": 5, "var": "df"}), None) for _ in files]

        stub = SimpleNamespace(report_files=found)
        with TemporaryDirectory() as tmpdir, patch.dict(modules, {"typedframes._rust_checker": stub}):
            (Path(tmpdir) / "a.py").write_text("x = 1\n")
            (Path(tmpdir) / "b.py").write_text("x = 1\n")

            # act
            _out, err, code = _run(["report", "quiet-amber", tmpdir, "--no-index"])

            # assert
            self.assertEqual(2, code)
            self.assertIn("names 2 sites", err)
            self.assertIn("a.py:6:5", err)
            self.assertIn("b.py:6:5", err)

    def test_should_say_why_there_is_no_snippet_for_a_site_too_large_to_paste(self) -> None:
        """Test the refusal path: the verdict is still printed, with the reason for no code."""
        # arrange
        values = "".join(f'        "n{i}",\n' for i in range(200))
        source = _PRIVATE.replace('sep=";")', f'sep=";",\n    na_values=[\n{values}    ],\n    )')
        with TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            (root / "job.py").write_text(source)
            name = _name_of(root)

            # act
            out, _err, code = _run(["report", name, str(root), "--no-index"])

            # assert
            self.assertIsNone(code)
            self.assertIn("No snippet: the code that decides this is", out)
            self.assertIn("- Outcome:", out)
            self.assertNotIn("```python", out)

    def test_should_drop_code_around_the_site_that_does_not_change_the_verdict(self) -> None:
        """Test that a long scope is cut down to the lines the checker's decision rests on."""
        # arrange
        source = _PRIVATE.replace("    return", "".join(f"    print({i})\n" for i in range(200)) + "    return")
        with TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            (root / "job.py").write_text(source)
            name = _name_of(root)

            # act
            out, _err, code = _run(["report", name, str(root), "--no-index"])

            # assert
            self.assertIsNone(code)
            self.assertIn("```python", out)
            self.assertNotIn("print(", out)

    def test_should_exit_1_when_the_extension_is_missing(self) -> None:
        """Test the same helpful message `check` gives without the Rust extension."""
        # arrange
        with TemporaryDirectory() as tmpdir, patch.dict(modules, {"typedframes._rust_checker": None}):
            # act
            _out, err, code = _run(["report", "quiet-amber", tmpdir])

            # assert
            self.assertEqual(1, code)
            self.assertIn("Rust checker extension was not found", err)

    def test_should_skip_a_file_the_checker_cannot_read_and_say_so(self) -> None:
        """Test that an unreadable file doesn't stop the search, and is named on stderr."""

        # arrange
        def unreadable(
            files: list[str], _name: str, _index: bytes | None = None
        ) -> list[tuple[str | None, str | None]]:
            return [(None, "permission denied") for _ in files]

        stub = SimpleNamespace(report_files=unreadable)
        with TemporaryDirectory() as tmpdir, patch.dict(modules, {"typedframes._rust_checker": stub}):
            (Path(tmpdir) / "job.py").write_text("x = 1\n")

            # act
            _out, err, code = _run(["report", "quiet-amber", tmpdir, "--no-index"])

            # assert
            self.assertEqual(2, code)
            self.assertIn("job.py: skipped, permission denied", err)
            self.assertIn("no untyped site is named 'quiet-amber'", err)

    def test_should_start_the_body_with_a_review_notice(self) -> None:
        """Test that the warning travels with the body, not only on stderr."""
        # arrange
        with TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            (root / "job.py").write_text(_PRIVATE)
            name = _name_of(root)

            # act
            out, _err, _code = _run(["report", name, str(root), "--no-index"])

            # assert
            self.assertTrue(out.startswith("<!-- Review before posting"))
            self.assertIn("kept as written", out.splitlines()[0])

    def test_should_not_print_numbers_or_quoted_column_names_from_the_source(self) -> None:
        """Test the end-to-end body for a site with a data-like number and an apostrophe in a column."""
        # arrange
        source = (
            "import pandas as pd\n\n\n"
            "def run():\n"
            '    raw = pd.read_json("x.json")\n'
            '    return raw["Customer\'s Name"], raw["amount"] * 0.0125 + 4111111111111111\n'
        )
        with TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            (root / "job.py").write_text(source)
            name = _name_of(root)

            # act
            out, _err, code = _run(["report", name, str(root), "--no-index"])

            # assert
            self.assertIsNone(code)
            for secret in ["Customer", "amount", "0.0125", "4111111111111111"]:
                self.assertNotIn(secret, out)
            self.assertIn("```python", out)

    def test_should_say_that_notebooks_are_not_covered(self) -> None:
        """Test that a notebook path isn't reported as an unknown name."""
        # arrange
        with TemporaryDirectory() as tmpdir:
            notebook = Path(tmpdir) / "n.ipynb"
            notebook.write_text("{}")

            # act
            _out, err, code = _run(["report", "quiet-amber", str(notebook)])

            # assert
            self.assertEqual(2, code)
            self.assertIn("`report` doesn't cover notebooks", err)

    def test_should_format_a_suggested_site_with_its_notes(self) -> None:
        """Test the Markdown layout for a suggestion with a note."""
        # arrange
        report = {
            "name": "quiet-amber",
            "verdict": {
                "outcome": "suggested",
                "kind": "usecols",
                "tier": "review",
                "reason": "only '_' is read later (line N)",
                "notes": ["module-level variable: another module may import it"],
            },
            "snippet": "x = 1\n",
            "refusal": None,
        }

        # act
        body = _format_report(report)

        # assert
        self.assertIn("- Outcome: suggested (`usecols`, review)", body)
        self.assertIn("- Note: module-level variable", body)
        self.assertIn("```python\nx = 1\n```", body)
        self.assertIn("### What you expected", body)

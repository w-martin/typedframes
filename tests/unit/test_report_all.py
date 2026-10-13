"""Tests for `typedframes report --all`."""

from json import loads
from pathlib import Path
from sys import modules
from tempfile import TemporaryDirectory
from types import SimpleNamespace
from unittest import TestCase
from unittest.mock import patch

from tests.unit.test_report import _run

_SEVERAL_SITES = (
    "import pandas as pd\n\n\n"
    "def one():\n"
    '    a = pd.read_csv("x.csv")\n'
    '    print(a["x"])\n'
    "    return keep(a)\n\n\n"
    "def two():\n"
    '    b = pd.read_csv("y.csv")\n'
    '    print(b["y"])\n'
    "    return keep_too(b)\n\n\n"
    "def three():\n"
    '    c = pd.read_csv("z.csv")\n'
    '    print(c["z"])\n'
    "    return c\n"
)


_CORPUS = (
    "import pandas as pd\n\n\n"
    "class Svc:\n"
    "    def make(self) -> pd.DataFrame:\n"
    '        return pd.DataFrame({"a": [1]})\n\n'
    "    def from_method(self):\n"
    "        df = self.make()\n"
    '        return df["a"]\n\n'
    "    def attr_columns(self):\n"
    "        out = pd.DataFrame(columns=[self.x, self.y])\n"
    "        out[self.x] = 1\n"
    "        return out\n\n\n"
    "def loaded():\n"
    '    df = pd.read_csv("a.csv")\n'
    '    return df["a"].sum()\n\n\n'
    "def merged(other):\n"
    '    df = pd.read_csv("b.csv")\n'
    '    print(df["a"])\n'
    "    return df.merge(other)\n"
)


class TestReportAll(TestCase):
    """`report --all`: every untyped site, grouped by why the checker decided as it did."""

    def test_should_group_sites_that_were_decided_the_same_way(self) -> None:
        """Test that the tally counts sites and groups, and each group lists its sites once."""
        # arrange
        with TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "job.py").write_text(_SEVERAL_SITES)

            # act
            out, err, code = _run(["report", "--all", tmpdir, "--no-index"])

            # assert
            self.assertIsNone(code)
            self.assertIn("## typedframes report: 3 untyped sites in 2 groups", out)
            self.assertIn("| 2 | withheld: can't see everything done with it: passed to `_` (line N) |", out)
            self.assertIn("| 1 | withheld: can't see everything done with it: returned (line N) |", out)
            self.assertEqual(2, out.count("```python"))
            self.assertIn("Read the report before posting it", err)
            for secret in ["keep_too", "x.csv", "job.py"]:
                self.assertNotIn(secret, out)

    def test_should_print_the_groups_as_json(self) -> None:
        """Test the machine-readable form: a count, and per group its reason, sites and example."""
        # arrange
        with TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "job.py").write_text(_SEVERAL_SITES)

            # act
            out, _err, code = _run(["report", "--all", tmpdir, "--no-index", "--format", "json"])

            # assert
            self.assertIsNone(code)
            payload = loads(out)
            self.assertEqual(3, payload["sites"])
            self.assertEqual([2, 1], [group["count"] for group in payload["groups"]])
            self.assertEqual(2, len(payload["groups"][0]["names"]))
            self.assertIn("read_csv", payload["groups"][0]["snippet"])

    def test_should_only_report_the_requested_outcome(self) -> None:
        """Test that `--outcome suggested` leaves out the withheld sites."""
        # arrange
        source = _SEVERAL_SITES + '\n\ndef four():\n    d = pd.read_csv("w.csv")\n    return d["w"]\n'
        with TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "job.py").write_text(source)

            # act
            out, _err, code = _run(["report", "--all", tmpdir, "--no-index", "--outcome", "suggested"])

            # assert
            self.assertIsNone(code)
            self.assertIn("1 untyped site in 1 group", out)
            self.assertIn("suggested:", out)
            self.assertNotIn("withheld", out)

    def test_should_write_one_body_per_group_into_a_directory(self) -> None:
        """Test that each file is a postable report, noting the other sites decided the same way."""
        # arrange
        with TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            (root / "job.py").write_text(_SEVERAL_SITES)
            target = root / "out"

            # act
            out, _err, code = _run(["report", "--all", str(root), "--no-index", "--output-dir", str(target)])

            # assert
            self.assertIsNone(code)
            files = sorted(target.glob("*.md"))
            self.assertEqual(2, len(files))
            self.assertEqual(files, [Path(line) for line in out.split()])
            self.assertIn("decided the same way at 1 other sites", files[0].read_text())
            self.assertNotIn("other sites", files[1].read_text())

    def test_should_say_when_there_are_no_untyped_sites(self) -> None:
        """Test the empty case: a message, not an empty report."""
        # arrange
        with TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "job.py").write_text("x = 1\n")

            # act
            out, _err, code = _run(["report", "--all", tmpdir, "--no-index"])

            # assert
            self.assertIsNone(code)
            self.assertEqual("No untyped sites.\n", out)

    def test_should_reject_a_site_name_together_with_all(self) -> None:
        """Test that `--all` takes a path, so a name as well is a usage error."""
        # arrange
        with TemporaryDirectory() as tmpdir:
            # act
            _out, err, code = _run(["report", "quiet-amber", tmpdir, "--all"])

            # assert
            self.assertEqual(2, code)
            self.assertIn("takes a path, not a site name", err)

    def test_should_require_a_site_name_without_all(self) -> None:
        """Test that a bare `report` says what it needs."""
        # arrange
        argv = ["report"]

        # act
        _out, err, code = _run(argv)

        # assert
        self.assertEqual(2, code)
        self.assertIn("needs a site name, or --all", err)

    def test_should_tell_apart_each_cause_the_checker_gave_up_on_a_site(self) -> None:
        """Test the baseline tally for a small corpus: one group per cause, so a fix moves a count."""
        # arrange
        with TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "job.py").write_text(_CORPUS)

            # act
            out, _err, code = _run(["report", "--all", tmpdir, "--no-index", "--format", "json"])

            # assert
            self.assertIsNone(code)
            payload = loads(out)
            self.assertEqual(3, payload["sites"])
            causes = sorted(f"{group['outcome']}: {group['reason']}" for group in payload["groups"])
            expected = [
                "suggested: only '_' is read later (line N)",
                "withheld: can't see everything done with it: `_` (line N)",
                "withheld: can't see everything done with it: assignment through this subscript (line N)",
            ]
            self.assertEqual(len(expected), len(causes))
            for cause, prefix in zip(causes, expected, strict=True):
                self.assertTrue(cause.startswith(prefix), cause)
            self.assertTrue(all(group["snippet"] for group in payload["groups"]))

    def test_should_say_why_a_group_has_no_snippet(self) -> None:
        """Test that a site too large to paste is still tallied, with the reason for no code."""
        # arrange
        values = "".join(f'        "n{i}",\n' for i in range(200))
        source = _SEVERAL_SITES.replace('.csv")', f'.csv",\n    na_values=[\n{values}    ],\n    )')
        with TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "job.py").write_text(source)

            # act
            out, _err, code = _run(["report", "--all", tmpdir, "--no-index", "--outcome", "withheld"])

            # assert
            self.assertIsNone(code)
            self.assertIn("No snippet: the code that decides this is", out)

    def test_should_exit_1_when_the_extension_is_missing(self) -> None:
        """Test the same helpful message `check` gives without the Rust extension."""
        # arrange
        with TemporaryDirectory() as tmpdir, patch.dict(modules, {"typedframes._rust_checker": None}):
            # act
            _out, err, code = _run(["report", "--all", tmpdir])

            # assert
            self.assertEqual(1, code)
            self.assertIn("Rust checker extension was not found", err)

    def test_should_skip_a_file_the_checker_cannot_read_and_say_so(self) -> None:
        """Test that an unreadable file doesn't stop the run, and is named on stderr."""

        # arrange
        def unreadable(files: list[str], _index: bytes | None = None) -> list[tuple[str | None, str | None]]:
            return [(None, "permission denied") for _ in files]

        stub = SimpleNamespace(report_all_files=unreadable)
        with TemporaryDirectory() as tmpdir, patch.dict(modules, {"typedframes._rust_checker": stub}):
            (Path(tmpdir) / "job.py").write_text("x = 1\n")

            # act
            out, err, code = _run(["report", "--all", tmpdir, "--no-index"])

            # assert
            self.assertIsNone(code)
            self.assertIn("job.py: skipped, permission denied", err)
            self.assertEqual("No untyped sites.\n", out)

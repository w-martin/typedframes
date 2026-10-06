"""Tests for `typedframes suggest --apply` / `--interactive`."""

from io import StringIO
from json import dumps
from pathlib import Path
from sys import modules
from tempfile import TemporaryDirectory
from types import SimpleNamespace
from unittest import TestCase
from unittest.mock import patch

from typedframes.cli import _ask, main

_SAFE = 'import pandas as pd\n\n\ndef run():\n    df = pd.read_csv("x.csv")\n    return df["a"] + df["b"]\n'
_SELECTED = _SAFE.replace("    return", '    df = df[["a", "b"]]\n    return')
_REVIEW = _SAFE.replace('    return df["a"] + df["b"]', '    if flag:\n        return df["b"]\n    return df["a"]')
_TWO = (
    "import pandas as pd\n\n\n"
    'def first():\n    df = pd.read_csv("x.csv")\n    return df["a"]\n\n\n'
    'def second():\n    df = pd.read_csv("y.csv")\n    return df["b"]\n'
)


def _run(argv: list[str], *, answers: list[str] | None = None) -> tuple[str, str, int | None]:
    """Run the CLI, returning stdout, stderr and the exit code (None for a normal return)."""
    out, err = StringIO(), StringIO()
    code: int | None = None
    inputs = iter(answers or [])
    with patch("sys.stdout", out), patch("sys.stderr", err), patch("builtins.input", lambda _prompt="": next(inputs)):
        try:
            main(argv)
        except SystemExit as exit_:
            code = exit_.code if isinstance(exit_.code, int) else 1
    return out.getvalue(), err.getvalue(), code


class TestSuggestApply(TestCase):
    """Applying suggestions to files."""

    def _project(self, source: str) -> tuple[TemporaryDirectory, Path]:
        tmp = TemporaryDirectory()
        root = Path(tmp.name)
        (root / "pyproject.toml").write_text("")
        job = root / "job.py"
        job.write_text(source)
        return tmp, job

    def test_should_apply_safe_suggestions_and_report_them(self) -> None:
        """Test that `--apply` edits the file and prints what it did."""
        tmp, job = self._project(_SAFE)
        with tmp:
            # act
            out, _err, code = _run(["suggest", str(job.parent), "--apply", "--allow-dirty", "--no-index"])

            # assert
            self.assertIsNone(code)
            self.assertEqual(job.read_text(), _SELECTED)
            self.assertIn('applied  job.py:5:5  df  ["a", "b"]', out)
            self.assertIn("Applied 1 suggestion in 1 file, 0 skipped.", out)

    def test_should_not_change_the_file_a_second_time(self) -> None:
        """Test that running `--apply` again finds nothing to do."""
        tmp, job = self._project(_SAFE)
        with tmp:
            _run(["suggest", str(job.parent), "--apply", "--allow-dirty", "--no-index"])

            # act
            out, _err, _code = _run(["suggest", str(job.parent), "--apply", "--allow-dirty", "--no-index"])

            # assert
            self.assertEqual(job.read_text(), _SELECTED)
            self.assertIn("Nothing to apply", out)

    def test_should_refuse_to_edit_a_tree_with_uncommitted_changes(self) -> None:
        """Test the version-control guard and that nothing is written."""
        tmp, job = self._project(_SAFE)
        with (
            tmp,
            patch("typedframes._rust_checker.vcs_problem", return_value="the git work tree has 2 uncommitted changes"),
        ):
            # act
            _out, err, code = _run(["suggest", str(job.parent), "--apply", "--no-index"])

            # assert
            self.assertEqual(code, 1)
            self.assertIn("uncommitted changes", err)
            self.assertEqual(job.read_text(), _SAFE)

    def test_should_not_edit_a_file_git_does_not_track_even_when_the_tree_is_clean(self) -> None:
        """Test that a clean tree still protects an untracked file from being rewritten."""
        tmp, job = self._project(_SAFE)
        with tmp, patch("typedframes._rust_checker.vcs_problem", return_value=None):
            # act
            _out, err, code = _run(["suggest", str(job.parent), "--apply", "--no-index"])

            # assert
            self.assertEqual(code, 1)
            self.assertEqual(job.read_text(), _SAFE)
            self.assertIn("not tracked by git", err)

    def test_should_only_require_tracked_files_without_allow_dirty(self) -> None:
        """Test that --allow-dirty switches the tracked-file requirement off."""
        result = dumps({"files": [], "failed": None})
        for flags, expected in (([], True), (["--allow-dirty"], False)):
            tmp, job = self._project(_SAFE)
            with (
                tmp,
                patch("typedframes._rust_checker.vcs_problem", return_value=None),
                patch("typedframes._rust_checker.apply_suggestions", return_value=result) as apply,
            ):
                # act
                _run(["suggest", str(job.parent), "--apply", "--no-index", *flags])

                # assert
                self.assertEqual(apply.call_args.args[2], expected)

    def test_should_not_apply_a_review_suggestion_with_apply(self) -> None:
        """Test that only the safe tier is applied without being asked about each one."""
        tmp, job = self._project(_REVIEW)
        with tmp:
            # act
            out, _err, _code = _run(["suggest", str(job.parent), "--apply", "--allow-dirty", "--no-index"])

            # assert
            self.assertEqual(job.read_text(), _REVIEW)
            self.assertIn("Nothing to apply", out)
            self.assertIn("--interactive", out)

    def test_should_ask_about_each_suggestion_and_apply_only_the_yeses(self) -> None:
        """Test `--interactive` with a no and a yes."""
        tmp, job = self._project(_TWO)
        with tmp:
            # act
            out, _err, _code = _run(
                ["suggest", str(job.parent), "-i", "--allow-dirty", "--no-index"], answers=["n", "y"]
            )

            # assert
            text = job.read_text()
            self.assertNotIn('df[["a"]]', text)
            self.assertIn('df[["b"]]', text)
            self.assertIn("Applied 1 suggestion", out)

    def test_should_apply_this_and_every_remaining_suggestion_on_a(self) -> None:
        """Test that answering `a` stops the questions."""
        tmp, job = self._project(_TWO)
        with tmp:
            # act
            _run(["suggest", str(job.parent), "-i", "--allow-dirty", "--no-index"], answers=["a"])

            # assert
            self.assertIn('df[["a"]]', job.read_text())
            self.assertIn('df[["b"]]', job.read_text())

    def test_should_change_nothing_on_quit_or_end_of_input(self) -> None:
        """Test that `q` and EOF leave every file as it was."""
        tmp, job = self._project(_TWO)
        with tmp:
            # act
            out, _err, _code = _run(
                ["suggest", str(job.parent), "-i", "--allow-dirty", "--no-index"], answers=["y", "q"]
            )

            # assert
            self.assertEqual(job.read_text(), _TWO)
            self.assertIn("Quit: nothing was changed.", out)
            self.assertNotIn("Nothing to apply", out)
        with patch("builtins.input", side_effect=EOFError):
            self.assertEqual(_ask("? "), "q")

    def test_should_not_point_an_interactive_user_at_interactive_mode(self) -> None:
        """Test that answering `n` to everything doesn't print the --apply hint about review tier."""
        tmp, job = self._project(_REVIEW)
        with tmp:
            # act
            out, _err, _code = _run(["suggest", str(job.parent), "-i", "--allow-dirty", "--no-index"], answers=["n"])

            # assert
            self.assertIn("Nothing to apply", out)
            self.assertNotIn("with --interactive", out)

    def test_should_ask_again_on_an_unrecognised_answer(self) -> None:
        """Test that anything but y/n/a/q is re-asked."""
        out = StringIO()
        with patch("sys.stdout", out), patch("builtins.input", side_effect=["maybe", " Y "]):
            # act
            answer = _ask("? ")

        # assert
        self.assertEqual(answer, "y")
        self.assertIn("Please answer y, n, a or q.", out.getvalue())

    def test_should_reject_apply_together_with_interactive(self) -> None:
        """Test that the two modes are mutually exclusive."""
        _out, err, code = _run(["suggest", ".", "--apply", "--interactive"])

        self.assertEqual(code, 2)
        self.assertIn("not allowed with", err)

    def test_should_report_a_failed_write_and_exit_nonzero(self) -> None:
        """Test that a write failure is an error, not a success."""
        tmp, job = self._project(_SAFE)
        failure = {"files": [], "failed": "could not write job.py: denied; every file was restored"}
        with tmp, patch("typedframes._rust_checker.apply_suggestions", return_value=dumps(failure)):
            # act
            _out, err, code = _run(["suggest", str(job.parent), "--apply", "--allow-dirty", "--no-index"])

            # assert
            self.assertEqual(code, 1)
            self.assertIn("every file was restored", err)

    def test_should_exit_1_when_a_file_could_not_be_read_but_still_apply_the_rest(self) -> None:
        """Test that an unreadable file is a failed run, not a silent skip."""
        tmp, job = self._project(_SAFE)
        with tmp:
            (job.parent / "latin.py").write_bytes(b"# caf\xe9\nx = 1\n")

            # act
            _out, err, code = _run(["suggest", str(job.parent), "--apply", "--allow-dirty", "--no-index"])

            # assert
            self.assertEqual(code, 1)
            self.assertIn("latin.py: skipped", err)
            self.assertEqual(job.read_text(), _SELECTED)

    def test_should_keep_exit_0_for_a_dry_run_that_skips_an_unreadable_file(self) -> None:
        """Test that a dry run reports an unreadable file the way `check` does."""
        tmp, job = self._project(_SAFE)
        with tmp:
            (job.parent / "latin.py").write_bytes(b"# caf\xe9\nx = 1\n")

            # act
            _out, err, code = _run(["suggest", str(job.parent), "--no-index"])

            # assert
            self.assertIsNone(code)
            self.assertIn("latin.py: skipped", err)

    def test_should_report_skipped_suggestions_and_files_that_errored(self) -> None:
        """Test the per-file reporting of a skipped suggestion and a file-level error."""
        tmp, job = self._project(_SAFE)
        result = {
            "files": [
                {
                    "file": "a.py",
                    "applied": [],
                    "skipped": [{"line": 3, "col": 1, "reason": "the edited file would not parse"}],
                    "errors_before": 0,
                    "errors_after": 0,
                    "written": False,
                    "error": None,
                },
                {
                    "file": "b.py",
                    "applied": [],
                    "skipped": [],
                    "errors_before": 0,
                    "errors_after": 0,
                    "written": False,
                    "error": "unreadable",
                },
            ],
            "failed": None,
        }
        with tmp, patch("typedframes._rust_checker.apply_suggestions", return_value=dumps(result)):
            # act
            out, err, code = _run(["suggest", str(job.parent), "--apply", "--allow-dirty", "--no-index"])

            # assert
            self.assertEqual(code, 1)
            self.assertIn("skipped  a.py:3:1  the edited file would not parse", out)
            self.assertIn("b.py: skipped, unreadable", err)
            self.assertIn("Applied 0 suggestions in 0 files, 1 skipped.", out)


class TestPartialExtension(TestCase):
    """An extension module missing the apply entry points exits with the usual message."""

    def _run_with(self, **attributes: object) -> tuple[str, int | None]:
        stub = SimpleNamespace(suggest_file=lambda *_a: '{"suggestions": [], "withheld": []}', **attributes)
        with TemporaryDirectory() as tmp, patch.dict(modules, {"typedframes._rust_checker": stub}):
            Path(tmp, "a.py").write_text("x = 1\n")
            _out, err, code = _run(["suggest", tmp, "--apply", "--no-index"])
        return err, code

    def test_should_exit_1_when_the_extension_cannot_check_the_work_tree(self) -> None:
        """Test the missing `vcs_problem` entry point."""
        err, code = self._run_with()

        self.assertEqual(code, 1)
        self.assertIn("Rust checker extension was not found", err)

    def test_should_exit_1_when_the_extension_cannot_apply(self) -> None:
        """Test the missing `apply_suggestions` entry point, given a suggestion to apply."""
        suggestion = {
            "kind": "select",
            "line": 1,
            "col": 1,
            "var": "df",
            "tier": "safe",
            "columns": ["a"],
            "reason": "r",
            "notes": [],
            "edits": [],
            "diff": "",
        }
        stub = SimpleNamespace(
            suggest_file=lambda *_a: dumps({"suggestions": [suggestion], "withheld": []}),
            vcs_problem=lambda _root: None,
        )
        with TemporaryDirectory() as tmp, patch.dict(modules, {"typedframes._rust_checker": stub}):
            Path(tmp, "a.py").write_text("x = 1\n")

            # act
            _out, err, code = _run(["suggest", tmp, "--apply", "--no-index"])

        # assert
        self.assertEqual(code, 1)
        self.assertIn("Rust checker extension was not found", err)

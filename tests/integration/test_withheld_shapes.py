"""Integration tests for what `report` and `suggest` say about the shapes in `tests/fixtures/withheld_shapes`."""

import json
from pathlib import Path
from typing import Any
from unittest import TestCase

from typedframes._binary import run_binary

FIXTURES = Path(__file__).resolve().parents[1] / "fixtures" / "withheld_shapes"


def _report_groups(*extra: str, path: Path = FIXTURES) -> list[dict[str, Any]]:
    completed = run_binary(["report", "--all", str(path), "--format", "json", "--no-cache", *extra], capture=True)
    if completed.returncode != 0:
        msg = f"typedframes report exited with status {completed.returncode}: {completed.stderr}"
        raise RuntimeError(msg)
    return json.loads(completed.stdout)["groups"]


class TestWithheldShapes(TestCase):
    """The reasons, snippets and suggestions for the repro files of shapes the checker once withheld."""

    def test_should_give_every_group_a_snippet(self) -> None:
        """Test that no group of the report is left without code to show."""
        # arrange
        groups = _report_groups()

        # act
        without_snippet = [group["reason"] for group in groups if not group["snippet"]]

        # assert
        self.assertEqual([], without_snippet)

    def test_should_name_what_kind_of_callee_a_frame_was_passed_to(self) -> None:
        """Test that a withheld reason for a call says what kind of callee it was."""
        # arrange
        groups = _report_groups("--outcome", "withheld")

        # act
        reasons = [group["reason"] for group in groups]

        # assert
        self.assertTrue(any("a method of another object" in reason for reason in reasons), reasons)

    def test_should_read_the_library_from_a_method_called_on_a_local_instance(self) -> None:
        """Test that a method called on a local instance resolves pandas or polars."""
        # arrange
        groups = _report_groups(path=FIXTURES / "unknown_library_sut.py")

        # act
        reasons = [group["reason"] for group in groups]

        # assert
        self.assertFalse(any("can't tell whether it is pandas or polars" in reason for reason in reasons), reasons)

    def test_should_suggest_for_a_helper_imported_relatively(self) -> None:
        """Test that a helper imported with a relative import is followed."""
        # arrange
        arguments = ["suggest", str(FIXTURES / "xfile"), "--no-cache"]

        # act
        completed = run_binary(arguments, capture=True)

        # assert
        self.assertEqual(0, completed.returncode, completed.stderr)
        self.assertIn("only 'price' is read later", completed.stdout)

    def test_should_list_frames_the_checker_does_not_track(self) -> None:
        """Test that `--outcome untracked` lists only untracked frames."""
        # arrange
        groups = _report_groups("--outcome", "untracked")

        # act
        outcomes = {group["outcome"] for group in groups}

        # assert
        self.assertEqual({"untracked"}, outcomes)

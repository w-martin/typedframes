"""Integration tests for the checker's experimental cuDF backend support.

These call the compiled `_rust_checker` Python extension directly -- the same
entry point the `typedframes` CLI and mypy plugin use -- against the real
example file, rather than testing the Rust crate in isolation via `cargo test`.
"""

import json
import unittest
from pathlib import Path

from typedframes._rust_checker import check_file  # ty: ignore[unresolved-import]

EXAMPLE_FILE = str(Path("examples/backends/cudf/example.py").absolute())


class TestCudfIntegration(unittest.TestCase):
    """Integration tests for cuDF column tracking through the Python extension."""

    def _errors(self) -> list[dict]:
        result = check_file(EXAMPLE_FILE, None)
        return json.loads(result)["errors"]

    def test_should_report_exactly_five_unknown_column_errors(self) -> None:
        """The example has five deliberate mistakes and no false positives elsewhere."""
        errors = self._errors()

        self.assertEqual(len(errors), 5)
        self.assertTrue(all(e["code"] == "unknown-column" for e in errors))
        self.assertTrue(all(e["severity"] == "error" for e in errors))

    def test_should_catch_unknown_column_via_annotated_return_type(self) -> None:
        """A column not in the Annotated[cudf.DataFrame, OrderSchema] return type is caught."""
        errors = self._errors()

        matching = [e for e in errors if "'revenue'" in e["message"]]
        self.assertEqual(len(matching), 1)
        self.assertIn("OrderSchema", matching[0]["message"])

    def test_should_suggest_typo_correction_for_inferred_columns(self) -> None:
        """A typo'd column against a `usecols=`-inferred set gets a suggestion."""
        errors = self._errors()

        matching = [e for e in errors if "'totl'" in e["message"]]
        self.assertEqual(len(matching), 1)
        self.assertIn("did you mean 'total'?", matching[0]["message"])

    def test_should_track_rename_and_assign_structural_operations(self) -> None:
        """rename() drops the old name and assign() adds the new one, pandas-style."""
        errors = self._errors()

        # 'total' was renamed away to 'gross' -- accessing it afterwards is unknown.
        matching = [e for e in errors if "'total'" in e["message"] and "gross" in e["message"]]
        self.assertEqual(len(matching), 1)

    def test_should_combine_schemas_on_merge(self) -> None:
        """merge() unions both sides' schemas; a column in neither is unknown."""
        errors = self._errors()

        matching = [e for e in errors if "'country'" in e["message"]]
        self.assertEqual(len(matching), 1)
        self.assertIn("OrderSchema_CustomerSchema", matching[0]["message"])

    def test_should_leave_read_sql_untracked_without_a_false_positive(self) -> None:
        """cudf.read_sql doesn't exist, so the checker stays silent rather than guessing."""
        errors = self._errors()

        self.assertFalse(any("anything" in e["message"] for e in errors))

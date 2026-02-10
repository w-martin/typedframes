"""Integration tests for typedframes linter."""

import subprocess
import unittest
from pathlib import Path

from typedframes_lint._rust_linter import check_file  # ty: ignore[unresolved-import]


class TestTypedFramesLinterIntegration(unittest.TestCase):
    """Integration tests for the Rust linter."""

    def test_should_detect_missing_column(self) -> None:
        """Test that the linter detects missing columns."""
        # arrange
        sut = Path("typedframes-lint/rust_typedframes_linter/target/debug/typedframes_linter").absolute()
        example_file = Path("examples/typedframes_example.py").absolute()

        # act
        result = subprocess.run(
            [str(sut), str(example_file)],
            capture_output=True,
            text=True,
            check=True,
        )

        # assert
        self.assertIn("Column 'wrong_column' does not exist", result.stdout)

    def test_should_suggest_typo_correction(self) -> None:
        """Test that the linter suggests corrections for typos."""
        # arrange
        sut = Path("typedframes-lint/rust_typedframes_linter/target/debug/typedframes_linter").absolute()
        example_file = Path("examples/typedframes_example.py").absolute()

        # act
        result = subprocess.run(
            [str(sut), str(example_file)],
            capture_output=True,
            text=True,
            check=True,
        )

        # assert
        self.assertIn("did you mean 'user_id'?", result.stdout)

    def test_should_track_mutations(self) -> None:
        """Test that the linter tracks column mutations."""
        # arrange
        sut = Path("typedframes-lint/rust_typedframes_linter/target/debug/typedframes_linter").absolute()
        example_file = Path("examples/typedframes_example.py").absolute()

        # act
        result = subprocess.run(
            [str(sut), str(example_file)],
            capture_output=True,
            text=True,
            check=True,
        )

        # assert
        self.assertIn("mutation tracking", result.stdout)

    def test_should_run_via_python_extension(self) -> None:
        """Test that the Rust linter works via Python extension."""
        # arrange
        example_file = str(Path("examples/typedframes_example.py").absolute())

        # act
        result = check_file(example_file)

        # assert
        self.assertIn("wrong_column", result)
        self.assertIn("does not exist", result)

    def test_should_warn_about_reserved_method_names(self) -> None:
        """Test that the linter warns about column names that shadow pandas/polars methods."""
        # arrange
        import json
        import tempfile

        source = """
from typedframes import BaseSchema, Column

class BadSchema(BaseSchema):
    mean = Column(type=float)
    user_id = Column(type=int)
"""
        with tempfile.NamedTemporaryFile(mode="w", suffix=".py", delete=False) as f:
            f.write(source)
            temp_file = f.name

        # act
        result = check_file(temp_file)
        errors = json.loads(result)

        # assert
        self.assertEqual(len(errors), 1)
        self.assertIn("mean", errors[0]["message"])
        self.assertIn("conflicts with a pandas/polars method", errors[0]["message"])

        # cleanup
        Path(temp_file).unlink()

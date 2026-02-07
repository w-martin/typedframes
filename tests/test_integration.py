"""Integration tests for typedframes linter."""

import subprocess
import unittest
from pathlib import Path

from typedframes._rust_linter import check_file  # ty: ignore[unresolved-import]


class TestTypedFramesLinterIntegration(unittest.TestCase):
    """Integration tests for the Rust linter."""

    def test_should_detect_missing_column(self) -> None:
        """Test that the linter detects missing columns."""
        # arrange
        sut = Path("rust_typedframes_linter/target/debug/typedframes_linter").absolute()
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
        sut = Path("rust_typedframes_linter/target/debug/typedframes_linter").absolute()
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
        sut = Path("rust_typedframes_linter/target/debug/typedframes_linter").absolute()
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

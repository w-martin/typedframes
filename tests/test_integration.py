"""Integration tests for typedframes linter."""

import os
import subprocess
import unittest
from pathlib import Path


class TestTypedFramesLinterIntegration(unittest.TestCase):
    """Integration tests for the Rust linter."""

    @classmethod
    def setUpClass(cls) -> None:
        """Build the Rust linter binary if needed."""
        binary_path = Path("rust_typedframes_linter/target/debug/typedframes_linter")
        if not binary_path.exists():
            subprocess.run(
                ["cargo", "build"],
                cwd="rust_typedframes_linter",
                check=True,
            )

    def test_should_detect_missing_column(self) -> None:
        """Test that the linter detects missing columns."""
        # arrange
        sut = Path("rust_typedframes_linter/target/debug/typedframes_linter").absolute()
        example_file = Path("examples/linter_test_example.py").absolute()

        # act
        result = subprocess.run(
            [str(sut), str(example_file)],
            capture_output=True,
            text=True,
            check=True,
        )

        # assert
        self.assertIn("Column 'name' does not exist in UserSchema", result.stdout)

    def test_should_suggest_typo_correction(self) -> None:
        """Test that the linter suggests corrections for typos."""
        # arrange
        sut = Path("rust_typedframes_linter/target/debug/typedframes_linter").absolute()
        example_file = Path("examples/linter_test_example.py").absolute()

        # act
        result = subprocess.run(
            [str(sut), str(example_file)],
            capture_output=True,
            text=True,
            check=True,
        )

        # assert
        self.assertIn("did you mean 'email'?", result.stdout)

    def test_should_track_mutations(self) -> None:
        """Test that the linter tracks column mutations."""
        # arrange
        sut = Path("rust_typedframes_linter/target/debug/typedframes_linter").absolute()
        example_file = Path("examples/linter_test_example.py").absolute()

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
        from typedframes._rust_linter import check_file

        example_file = str(Path("examples/linter_test_example.py").absolute())

        # act
        result = check_file(example_file)

        # assert
        self.assertIn("name", result)
        self.assertIn("does not exist", result)


if __name__ == "__main__":
    unittest.main()

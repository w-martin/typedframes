import os
import subprocess
import unittest
from pathlib import Path


class TestPandasLinterIntegration(unittest.TestCase):
    def test_should_lint_example_file(self) -> None:
        # arrange
        example_file = Path("examples/typeddict_example.py").absolute()
        binary_path = Path(
            "rust_typedframes_linter/target/debug/rust_typedframes_linter",
        ).absolute()

        if not binary_path.exists():
            subprocess.run(["cargo", "build"], cwd="rust_typedframes_linter", check=True)

        # act
        result = subprocess.run(
            [str(binary_path), str(example_file)],
            capture_output=True,
            text=True,
            check=True,
        )

        # assert
        self.assertIn("Column 'name' does not exist in UserSchema", result.stdout)

    def test_should_run_mypy_with_plugin(self) -> None:
        # arrange
        example_file = "examples/typeddict_example.py"
        # act
        # Ensure the plugin can find the binary for development
        env = os.environ.copy()
        env["PYTHONPATH"] = "."
        result = subprocess.run(["mypy", example_file], capture_output=True, text=True, check=False, env=env)

        # assert
        self.assertIn("Column 'name' does not exist in UserSchema", result.stdout)
        self.assertEqual(result.returncode, 1)

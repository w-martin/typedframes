"""Regression tests for typedframes mypy plugin integration."""

import os
import shutil
import subprocess
import unittest
from pathlib import Path


class TestPluginRegression(unittest.TestCase):
    """Regression tests for the mypy plugin."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        self.binary_path = Path(
            "rust_typedframes_linter/target/debug/typedframes_linter",
        ).absolute()
        if not self.binary_path.exists():
            subprocess.run(["cargo", "build"], cwd="rust_typedframes_linter", check=True)

        self.temp_dir = Path("temp_test_dir")
        self.temp_dir.mkdir(exist_ok=True)
        self.test_py = self.temp_dir / "test_missing.py"
        self.test_py.write_text("""
from typedframes import BaseSchema, Column

class UserSchema(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)

df: "DataFrame[UserSchema]" = load_users()  # type: ignore[name-defined]
print(df["non_existent"])
""")

        # Set up environment with PYTHONPATH
        self.env = os.environ.copy()
        src_path = str(Path("src").absolute())
        if "PYTHONPATH" in self.env:
            self.env["PYTHONPATH"] = f"{src_path}:{self.env['PYTHONPATH']}"
        else:
            self.env["PYTHONPATH"] = src_path

    def tearDown(self) -> None:
        """Clean up test fixtures."""
        if self.temp_dir.exists():
            shutil.rmtree(self.temp_dir)

    def test_should_not_catch_errors_without_plugin(self) -> None:
        """Test that mypy alone doesn't catch column errors."""
        # arrange
        empty_config = self.temp_dir / "empty_mypy.ini"
        empty_config.write_text("[mypy]\nignore_missing_imports = true")

        # act
        result = subprocess.run(
            [
                "mypy",
                "--config-file",
                str(empty_config),
                "--disable-error-code",
                "type-arg",
                str(self.test_py),
            ],
            capture_output=True,
            text=True,
            check=False,
            env=self.env,
        )

        # assert
        self.assertNotIn("Column 'non_existent' does not exist", result.stdout)
        self.assertEqual(result.returncode, 0)

    def test_should_catch_errors_with_plugin(self) -> None:
        """Test that mypy with the plugin catches column errors."""
        # act
        result = subprocess.run(
            [
                "mypy",
                "--config-file",
                "pyproject.toml",
                "--disable-error-code",
                "type-arg",
                str(self.test_py),
            ],
            capture_output=True,
            text=True,
            check=False,
            env=self.env,
        )

        # assert
        self.assertIn("Column 'non_existent' does not exist in UserSchema", result.stdout)
        self.assertEqual(result.returncode, 1)


if __name__ == "__main__":
    unittest.main()

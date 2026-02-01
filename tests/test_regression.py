import shutil
import subprocess
import unittest
from pathlib import Path


class TestPluginRegression(unittest.TestCase):
    def setUp(self) -> None:
        self.example_file = Path("examples/typeddict_example.py").absolute()
        self.binary_path = Path(
            "rust_typedframes_linter/target/debug/rust_typedframes_linter",
        ).absolute()
        if not self.binary_path.exists():
            subprocess.run(["cargo", "build"], cwd="rust_typedframes_linter", check=True)

        # Create a temporary pyproject.toml without the plugin for regression testing
        self.temp_dir = Path("temp_test_dir")
        self.temp_dir.mkdir(exist_ok=True)
        self.test_py = self.temp_dir / "test_missing.py"
        self.test_py.write_text("""
from typing import TypedDict
from pandas import DataFrame

class UserSchema(TypedDict):
    user_id: int
    email: str

def load_users() -> "DataFrame[UserSchema]":
    return DataFrame({"user_id": [1], "email": ["a@b.com"]})

df = load_users()
print(df["non_existent"])
""")

    def tearDown(self) -> None:
        if self.temp_dir.exists():
            shutil.rmtree(self.temp_dir)

    def test_should_not_catch_errors_without_plugin(self) -> None:
        # act
        # Run mypy with an empty config to avoid loading the root pyproject.toml
        empty_config = self.temp_dir / "empty_mypy.ini"
        empty_config.write_text("[mypy]")
        result = subprocess.run(
            [
                "mypy",
                "--config-file",
                str(empty_config),
                "--ignore-missing-imports",
                "--disable-error-code",
                "type-arg",
                str(self.test_py),
            ],
            capture_output=True,
            text=True,
            check=False,
        )

        # assert
        # mypy by itself doesn't catch the missing column
        assert "Column 'non_existent' does not exist" not in result.stdout
        assert result.returncode == 0

    def test_should_catch_errors_with_plugin(self) -> None:
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
        )

        # assert
        assert "Column 'non_existent' does not exist in UserSchema" in result.stdout
        assert result.returncode == 1


if __name__ == "__main__":
    unittest.main()

import json
import unittest
from pathlib import Path
from unittest.mock import MagicMock, patch

from mypy.options import Options

from pandas_column_linter.mypy import PandasLinterPlugin


class TestPandasLinterPluginUnit(unittest.TestCase):
    def setUp(self) -> None:
        # arrange
        self.plugin = PandasLinterPlugin(Options())
        self.test_file = "test.py"
        self.error_data = [{"line": 10, "message": "Column 'foo' does not exist"}]

    def test_should_report_error_on_exact_line_match(self) -> None:
        # arrange
        with (
            patch("pandas_column_linter.mypy.get_project_root") as mock_root,
            patch("pandas_column_linter.mypy.is_enabled") as mock_enabled,
            patch("subprocess.run") as mock_run,
            patch("os.path.exists") as mock_exists,
        ):
            mock_exists.return_value = True
            mock_enabled.return_value = True
            mock_root.return_value = Path()

            mock_process = MagicMock()
            mock_process.returncode = 0
            mock_process.stdout = json.dumps(self.error_data)
            mock_run.return_value = mock_process

            context = MagicMock()
            context.api.path = self.test_file
            context.context.line = 10

            # act
            self.plugin.check_pandas_access(context)

            # assert
            context.api.fail.assert_called_once_with(
                self.error_data[0]["message"],
                context.context,
            )

        # Test non-match branch
        new_plugin = PandasLinterPlugin(Options())  # New plugin to avoid cache
        with (
            patch("pandas_column_linter.mypy.get_project_root") as mock_root,
            patch("pandas_column_linter.mypy.is_enabled") as mock_enabled,
            patch("subprocess.run") as mock_run,
            patch("os.path.exists") as mock_exists,
        ):
            mock_exists.return_value = True
            mock_enabled.return_value = True
            mock_root.return_value = Path()

            mock_process = MagicMock()
            mock_process.returncode = 0
            mock_process.stdout = json.dumps([])
            mock_run.return_value = mock_process

            context = MagicMock()
            context.api.path = "other.py"  # Different file to be sure
            context.context.line = 10
            context.api.fail = MagicMock()

            # act
            new_plugin.check_pandas_access(context)

            # assert
            context.api.fail.assert_not_called()

    def test_should_report_error_on_fuzzy_line_match(self) -> None:
        # arrange
        with (
            patch("pandas_column_linter.mypy.get_project_root") as mock_root,
            patch("pandas_column_linter.mypy.is_enabled") as mock_enabled,
            patch("subprocess.run") as mock_run,
            patch("os.path.exists") as mock_exists,
        ):
            mock_exists.return_value = True
            mock_enabled.return_value = True
            mock_root.return_value = Path()

            mock_process = MagicMock()
            mock_process.returncode = 0
            mock_process.stdout = json.dumps(self.error_data)
            mock_run.return_value = mock_process

            context = MagicMock()
            context.api.path = self.test_file
            context.context.line = 11  # One line off

            # act
            self.plugin.check_pandas_access(context)

            # assert
            context.api.fail.assert_called_once_with(
                self.error_data[0]["message"],
                context.context,
            )

    def test_should_return_default_when_no_path(self) -> None:
        # arrange
        context = MagicMock()
        context.api.path = None
        context.default_return_type = MagicMock()

        # act
        result = self.plugin.check_pandas_access(context)

        # assert
        assert result == context.default_return_type

    def test_should_not_run_when_disabled(self) -> None:
        # arrange
        with patch("pandas_column_linter.mypy.is_enabled") as mock_enabled:
            mock_enabled.return_value = False

            # act
            errors = self.plugin._run_linter(self.test_file)

            # assert
            assert errors == []

    def test_should_return_cached_results(self) -> None:
        # arrange
        self.plugin._linter_results[self.test_file] = self.error_data

        # act
        errors = self.plugin._run_linter(self.test_file)

        # assert
        assert errors == self.error_data

    def test_should_return_empty_for_ignored_paths(self) -> None:
        # arrange/act/assert
        assert self.plugin._run_linter("") == []
        assert self.plugin._run_linter("site-packages/foo.py") == []
        assert self.plugin._run_linter("foo.pyi") == []

    def test_should_handle_missing_binary(self) -> None:
        # arrange
        with patch("os.path.exists") as mock_exists:
            mock_exists.return_value = False

            # act
            errors = self.plugin._run_linter(self.test_file)

            # assert
            assert errors == []

    def test_should_handle_subprocess_error(self) -> None:
        # arrange
        with (
            patch("os.path.exists") as mock_exists,
            patch("subprocess.run") as mock_run,
        ):
            mock_exists.return_value = True
            mock_run.side_effect = Exception("error")

            # act
            errors = self.plugin._run_linter(self.test_file)

            # assert
            assert errors == []

    def test_get_method_hook(self) -> None:
        # arrange/act/assert
        assert (
            self.plugin.get_method_hook("pandas.core.frame.DataFrame.__getitem__")
            == self.plugin.check_pandas_access
        )
        assert (
            self.plugin.get_method_hook("pandas.core.frame.DataFrame.__setitem__")
            == self.plugin.check_pandas_access
        )
        assert self.plugin.get_method_hook("other") is None

    def test_plugin_function(self) -> None:
        # arrange/act/assert
        from pandas_column_linter.mypy import plugin

        assert plugin("1.0") == PandasLinterPlugin

    def test_plugin_should_handle_unsuccessful_subprocess(self) -> None:
        # arrange
        new_plugin = PandasLinterPlugin(Options())
        with (
            patch("os.path.exists") as mock_exists,
            patch("subprocess.run") as mock_run,
            patch("pandas_column_linter.mypy.get_project_root") as mock_root,
            patch("pandas_column_linter.mypy.is_enabled") as mock_enabled,
        ):
            mock_exists.return_value = True
            mock_root.return_value = Path()
            mock_enabled.return_value = True

            mock_process = MagicMock()
            mock_process.returncode = 1
            mock_run.return_value = mock_process

            # act
            errors = new_plugin._run_linter(self.test_file)

            # assert
            assert errors == []


class TestUtilsUnit(unittest.TestCase):
    def test_get_project_root(self) -> None:
        # arrange
        from pandas_column_linter.mypy import get_project_root

        with (
            patch("pathlib.Path.exists") as mock_exists,
            patch("pathlib.Path.is_file") as mock_is_file,
        ):
            # First call: current is file, pop to parent.
            # Then pyproject.toml exists in grandparent.
            mock_is_file.return_value = True
            mock_exists.side_effect = [False, True]

            # act
            root = get_project_root(Path("/a/b/c.py"))

            # assert
            assert root == Path("/a")

        # Test case where it reaches root without finding pyproject.toml
        with (
            patch("pathlib.Path.exists") as mock_exists,
            patch("pathlib.Path.is_file") as mock_is_file,
        ):
            mock_is_file.return_value = False
            mock_exists.return_value = False

            # act
            root = get_project_root(Path("/"))

            # assert
            assert root == Path("/")

    def test_is_enabled(self) -> None:
        # arrange
        import io

        from pandas_column_linter.mypy import is_enabled

        # Test missing config
        with patch("pathlib.Path.exists") as mock_exists:
            mock_exists.return_value = False
            assert is_enabled(Path("/tmp"))

        # Test enabled
        with (
            patch("pathlib.Path.exists") as mock_exists,
            patch(
                "builtins.open",
                return_value=io.BytesIO(b"[tool.pandas_column_linter]\nenabled = true"),
            ),
        ):
            mock_exists.return_value = True
            assert is_enabled(Path("/tmp"))

        # Test disabled
        with (
            patch("pathlib.Path.exists") as mock_exists,
            patch(
                "builtins.open",
                return_value=io.BytesIO(
                    b"[tool.pandas_column_linter]\nenabled = false",
                ),
            ),
        ):
            mock_exists.return_value = True
            assert not is_enabled(Path("/tmp"))

        # Test exception
        with (
            patch("pathlib.Path.exists") as mock_exists,
            patch("builtins.open", side_effect=Exception()),
        ):
            mock_exists.return_value = True
            assert is_enabled(Path("/tmp"))

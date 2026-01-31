"""Mypy plugin for pandas column linting."""

import json
import os
import subprocess
import tomllib
from collections.abc import Callable
from pathlib import Path
from typing import TYPE_CHECKING, Any

from mypy.plugin import MethodContext, Plugin
from mypy.types import Type

if TYPE_CHECKING:
    from mypy.types import Type


def get_project_root(start_path: Path) -> Path:
    """Find the project root by looking for pyproject.toml."""
    current = start_path.resolve()
    if current.is_file():
        current = current.parent
    for parent in [current, *list(current.parents)]:
        if (parent / "pyproject.toml").exists():
            return parent
    return current


def is_enabled(project_root: Path) -> bool:
    """Check if the linter is enabled in pyproject.toml."""
    config_path = project_root / "pyproject.toml"
    if not config_path.exists():
        return True
    try:
        with open(config_path, "rb") as f:
            config = tomllib.load(f)
            enabled = (
                config.get("tool", {})
                .get("pandas_column_linter", {})
                .get("enabled", True)
            )
            return bool(enabled)
    except Exception:  # noqa: BLE001
        return True


class PandasLinterPlugin(Plugin):
    """Mypy plugin to integrate the Rust pandas linter."""

    def __init__(self, *args: Any, **kwargs: Any) -> None:
        """Initialize the plugin."""
        super().__init__(*args, **kwargs)
        self._linter_results: dict[str, list[dict[str, Any]]] = {}

    def _run_linter(self, file_path: str) -> list[dict[str, Any]]:
        """Run the Rust linter on the given file."""
        if file_path in self._linter_results:
            return self._linter_results[file_path]

        if not file_path or "site-packages" in file_path or file_path.endswith(".pyi"):
            return []

        path_obj = Path(file_path)
        project_root = get_project_root(path_obj)
        if not is_enabled(project_root):
            return []

        try:
            import pandas_column_linter

            # Use getattr to avoid mypy errors with the dynamic extension module
            # since it might not be present during static analysis.
            extension = getattr(pandas_column_linter, "rust_pandas_linter", None)
            if extension is None:
                raise ImportError("rust_pandas_linter not found")

            result_json = str(extension.check_file(file_path))
            errors_from_extension: list[dict[str, Any]] = json.loads(result_json)
            self._linter_results[file_path] = errors_from_extension
        except Exception:  # noqa: BLE001
            # Fallback to local development paths if installed as editable or binary not
            # in site-packages
            try:
                # Try to find the binary in the package's bin directory first
                # (when installed as a package)
                current_dir = Path(__file__).parent
                binary_path = current_dir / "bin" / "rust_pandas_linter"
                if os.name == "nt":
                    binary_path = binary_path.with_suffix(".exe")

                # Fallback to local development paths
                if not binary_path.exists():
                    binary_path = (
                        current_dir.parent
                        / "rust_pandas_linter"
                        / "target"
                        / "release"
                        / "rust_pandas_linter"
                    )
                    if os.name == "nt":
                        binary_path = binary_path.with_suffix(".exe")

                if not binary_path.exists():
                    binary_path = (
                        current_dir.parent
                        / "rust_pandas_linter"
                        / "target"
                        / "debug"
                        / "rust_pandas_linter"
                    )
                    if os.name == "nt":
                        binary_path = binary_path.with_suffix(".exe")

                if not binary_path.exists():
                    return []

                result = subprocess.run(
                    [str(binary_path), file_path],
                    capture_output=True,
                    text=True,
                    check=False,
                )
                if result.returncode == 0:
                    errors: list[dict[str, Any]] = json.loads(result.stdout)
                    self._linter_results[file_path] = errors
                    return errors
            except Exception:  # noqa: BLE001, S110
                pass
        else:
            return errors_from_extension
        return []

    def get_method_hook(
        self,
        fullname: str,
    ) -> Callable[[MethodContext], "Type"] | None:
        """Return a hook for pandas access methods."""
        # We target the common methods where column access happens
        if fullname.endswith((".__getitem__", ".__setitem__")):
            return self.check_pandas_access
        return None

    def check_pandas_access(self, context: MethodContext) -> "Type":
        """Check if the pandas column access is valid."""
        # Determine the file path of the current module being checked
        file_path = getattr(context.api, "path", None)
        if not file_path:
            return context.default_return_type

        errors = self._run_linter(file_path)

        line = context.context.line

        # Multiple errors might happen on the same line
        # (though unlikely for pandas access in one statement)
        # but let's be thorough.
        matched = False
        for err in errors:
            if err["line"] == line:
                context.api.fail(err["message"], context.context)
                matched = True

        if not matched:
            # In some cases mypy context line might be slightly different
            # from parser line if it's a multi-line expression.
            # Let's try to match within a small range if no exact match.
            for err in errors:
                if abs(err["line"] - line) <= 1:
                    context.api.fail(err["message"], context.context)

        return context.default_return_type


def plugin(version: str) -> type[PandasLinterPlugin]:  # noqa: ARG001
    """Entry point for the mypy plugin."""
    return PandasLinterPlugin

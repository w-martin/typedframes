"""Mypy plugin for typedframes column linting."""

import json
import tomllib
from collections.abc import Callable
from pathlib import Path
from typing import TYPE_CHECKING, Any

from mypy.plugin import MethodContext, Plugin

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
        with config_path.open("rb") as f:
            config = tomllib.load(f)
            enabled = config.get("tool", {}).get("typedframes", {}).get("enabled", True)
            return bool(enabled)
    except Exception:  # noqa: BLE001
        return True


class LinterNotFoundError(Exception):
    """Raised when the typedframes linter cannot be found or executed."""


class TypedFramesPlugin(Plugin):
    """Mypy plugin to integrate the typedframes Rust linter."""

    def __init__(self, *args: Any, **kwargs: Any) -> None:
        """Initialize the plugin."""
        super().__init__(*args, **kwargs)
        self._linter_results: dict[str, list[dict[str, Any]]] = {}

    def _run_via_extension(self, file_path: str) -> list[dict[str, Any]]:
        """Run the linter via the Rust extension module."""
        from typedframes._rust_linter import check_file  # ty: ignore[unresolved-import]

        result_json = str(check_file(file_path))
        return json.loads(result_json)

    def _run_linter(self, file_path: str) -> list[dict[str, Any]]:
        """Run the Rust linter on the given file."""
        if file_path in self._linter_results:
            return self._linter_results[file_path]

        if not file_path or "site-packages" in file_path or file_path.endswith(".pyi"):
            return []

        project_root = get_project_root(Path(file_path))
        if not is_enabled(project_root):
            return []

        try:
            errors = self._run_via_extension(file_path)
        except ImportError as e:
            msg = (
                "typedframes linter extension not found. "
                "Ensure the package is properly installed with 'uv add typedframes' "
                "or build from source with 'uv run maturin develop'."
            )
            raise LinterNotFoundError(msg) from e

        self._linter_results[file_path] = errors
        return errors

    def get_method_hook(
        self,
        fullname: str,
    ) -> Callable[[MethodContext], "Type"] | None:
        """Return a hook for DataFrame access methods."""
        # Target common methods where column access happens
        if fullname.endswith((".__getitem__", ".__setitem__")):
            return self.check_column_access
        return None

    def check_column_access(self, context: MethodContext) -> "Type":
        """Check if the DataFrame column access is valid."""
        file_path = getattr(context.api, "path", None)
        if not file_path:
            return context.default_return_type

        errors = self._run_linter(file_path)
        line = context.context.line

        matched = False
        for err in errors:
            if err["line"] == line:
                context.api.fail(err["message"], context.context)
                matched = True

        if not matched:
            # Try fuzzy matching for multi-line expressions
            for err in errors:
                if abs(err["line"] - line) <= 1:
                    context.api.fail(err["message"], context.context)

        return context.default_return_type


# Keep old name for backwards compatibility
PandasLinterPlugin = TypedFramesPlugin


def plugin(version: str) -> type[TypedFramesPlugin]:  # noqa: ARG001
    """Entry point for the mypy plugin."""
    return TypedFramesPlugin

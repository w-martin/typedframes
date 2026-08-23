"""Mypy plugin for typedframes column checking."""

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
    """Check if the checker is enabled in pyproject.toml."""
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


class CheckerNotFoundError(Exception):
    """Raised when the typedframes checker cannot be found or executed."""


class TypedFramesPlugin(Plugin):
    """Mypy plugin to integrate the typedframes Rust checker."""

    def __init__(self, *args: Any, **kwargs: Any) -> None:
        """Initialize the plugin."""
        super().__init__(*args, **kwargs)
        self._checker_results: dict[str, list[dict[str, Any]]] = {}
        self._index_bytes_by_root: dict[str, bytes | None] = {}

    def _get_index_bytes(self, project_root: Path) -> bytes | None:
        """Build and cache the project index as MessagePack bytes, keyed by project root."""
        key = str(project_root)
        if key not in self._index_bytes_by_root:
            try:
                from typedframes._rust_checker import build_project_index

                self._index_bytes_by_root[key] = build_project_index(str(project_root))
            except ImportError:
                self._index_bytes_by_root[key] = None
        return self._index_bytes_by_root[key]

    def _run_via_extension(self, file_path: str, index_bytes: bytes | None) -> list[dict[str, Any]]:
        """Run the checker via the Rust extension module."""
        from typedframes._rust_checker import check_file

        result_json = str(check_file(file_path, index_bytes))
        result: dict[str, Any] = json.loads(result_json)
        return result["errors"]

    def _run_checker(self, file_path: str) -> list[dict[str, Any]]:
        """Run the Rust checker on the given file."""
        if file_path in self._checker_results:
            return self._checker_results[file_path]

        if not file_path or "site-packages" in file_path or file_path.endswith(".pyi"):
            return []

        project_root = get_project_root(Path(file_path))
        if not is_enabled(project_root):
            return []

        index_bytes = self._get_index_bytes(project_root)

        try:
            errors = self._run_via_extension(file_path, index_bytes)
        except ImportError as e:
            msg = (
                "typedframes checker extension not found. "
                "Ensure the package is properly installed with 'uv add typedframes' "
                "or build from source with 'uv run maturin develop'."
            )
            raise CheckerNotFoundError(msg) from e

        self._checker_results[file_path] = errors
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

        errors = self._run_checker(file_path)
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


def plugin(version: str) -> type[TypedFramesPlugin]:  # noqa: ARG001
    """Entry point for the mypy plugin."""
    return TypedFramesPlugin

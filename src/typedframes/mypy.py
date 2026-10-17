"""Mypy plugin for typedframes column checking."""

import json
import tomllib
from collections.abc import Callable
from pathlib import Path
from typing import TYPE_CHECKING, Any

from mypy.plugin import MethodContext, Plugin

from typedframes._binary import BinaryNotFoundError, run_binary

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
    except (OSError, tomllib.TOMLDecodeError):
        return True


class CheckerNotFoundError(Exception):
    """Raised when the typedframes checker cannot be found or executed."""


class CheckerFailedError(Exception):
    """Raised when the typedframes checker ran but did not produce a result."""


class TypedFramesPlugin(Plugin):
    """Mypy plugin to integrate the typedframes Rust checker."""

    def __init__(self, *args: Any, **kwargs: Any) -> None:
        """Initialize the plugin."""
        super().__init__(*args, **kwargs)
        self._errors_by_root: dict[str, dict[str, list[dict[str, Any]]]] = {}
        # The hook runs for every subscript in a file, so a file's answer is worked out once.
        self._errors_by_file: dict[str, list[dict[str, Any]]] = {}

    def _project_errors(self, project_root: Path) -> dict[str, list[dict[str, Any]]]:
        """Every file's diagnostics for a project, by file path: the checker runs once per project root.

        Without a `pyproject.toml` a file's own directory is its root, so a root inside one that was
        already checked is served from that run.
        """
        key = str(project_root)
        for checked, errors in self._errors_by_root.items():
            if project_root.is_relative_to(checked):
                return errors
        try:
            completed = run_binary(["check", key, "--output-format", "json"], capture=True)
        except BinaryNotFoundError as e:
            msg = (
                "typedframes checker not found. "
                "Ensure the package is properly installed with 'uv add typedframes' "
                "or build from source with 'uv run inv build'."
            )
            raise CheckerNotFoundError(msg) from e
        if completed.returncode != 0:
            msg = f"typedframes check exited with status {completed.returncode}: {completed.stderr.strip()}"
            raise CheckerFailedError(msg)
        by_file: dict[str, list[dict[str, Any]]] = {}
        for error in json.loads(completed.stdout)["errors"]:
            by_file.setdefault(error["file"], []).append(error)
        self._errors_by_root[key] = by_file
        return by_file

    def _run_checker(self, file_path: str) -> list[dict[str, Any]]:
        """The checker's diagnostics for the given file."""
        if file_path in self._errors_by_file:
            return self._errors_by_file[file_path]

        if not file_path or "site-packages" in file_path or file_path.endswith(".pyi"):
            return []

        project_root = get_project_root(Path(file_path))
        if is_enabled(project_root):
            errors = self._project_errors(project_root).get(str(Path(file_path).resolve()), [])
        else:
            errors = []
        self._errors_by_file[file_path] = errors
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


def plugin(_version: str) -> type[TypedFramesPlugin]:
    """Entry point for the mypy plugin."""
    return TypedFramesPlugin

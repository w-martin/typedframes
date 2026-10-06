"""Where the on-disk index cache lives, and the housekeeping around it.

The Rust checker reuses a previous run's per-file index for every file whose source is
unchanged. This module only decides *where* that cache is kept and offers the commands
for finding, clearing and gitignoring it; the cache's contents are Rust's business.
"""

from __future__ import annotations

from os import environ
from pathlib import Path
from sys import platform
from tomllib import TOMLDecodeError, load

ENV_VAR = "TYPEDFRAMES_CACHE_DIR"
CACHE_FILE_PATTERNS = ("index-*.bin", "index-*.tmp*")


def default_cache_dir() -> Path:
    """The per-user cache directory for this platform."""
    if platform == "darwin":
        return Path.home() / "Library" / "Caches" / "typedframes"
    if platform == "win32":
        base = environ.get("LOCALAPPDATA")
        return (Path(base) if base else Path.home() / "AppData" / "Local") / "typedframes" / "Cache"
    base = environ.get("XDG_CACHE_HOME")
    return (Path(base) if base else Path.home() / ".cache") / "typedframes"


def project_root_for(path: Path) -> Path | None:
    """The nearest directory at or above `path` that holds a `pyproject.toml`."""
    start = path.resolve()
    for candidate in (start, *start.parents):
        if candidate.is_dir() and (candidate / "pyproject.toml").is_file():
            return candidate
    return None


def configured_cache_dir(root: Path) -> Path | None:
    """`cache_dir` from `[tool.typedframes]` in the project's pyproject.toml, if set.

    A relative value is relative to the project root.
    """
    try:
        with (root / "pyproject.toml").open("rb") as f:
            data = load(f)
    except (OSError, TOMLDecodeError):
        return None
    value = data.get("tool", {}).get("typedframes", {}).get("cache_dir")
    if not isinstance(value, str) or not value:
        return None
    configured = Path(value).expanduser()
    return configured if configured.is_absolute() else root / configured


def locate_cache_dir(path: Path, *, cli_value: Path | None = None) -> Path:
    """The cache directory for a run over `path`.

    In order: `--cache-dir`, the `TYPEDFRAMES_CACHE_DIR` environment variable,
    `cache_dir` in `[tool.typedframes]`, then the per-user default.
    """
    if cli_value is not None:
        return cli_value.expanduser().resolve()
    from_env = environ.get(ENV_VAR)
    if from_env:
        return Path(from_env).expanduser().resolve()
    root = project_root_for(path)
    if root is not None:
        configured = configured_cache_dir(root)
        if configured is not None:
            return configured.resolve()
    return default_cache_dir()


def resolve_cache_dir(path: Path, *, cli_value: Path | None = None, disabled: bool = False) -> Path | None:
    """`locate_cache_dir`, or `None` when `--no-cache` turned caching off."""
    return None if disabled else locate_cache_dir(path, cli_value=cli_value)


def cache_files(cache_dir: Path) -> list[Path]:
    """The files in `cache_dir` that this tool wrote -- never anything else in it."""
    if not cache_dir.is_dir():
        return []
    return sorted({f for pattern in CACHE_FILE_PATTERNS for f in cache_dir.glob(pattern) if f.is_file()})


def gitignore_entry(cache_dir: Path, root: Path) -> str | None:
    """The `.gitignore` line for `cache_dir`, or `None` when it is outside the project."""
    try:
        relative = cache_dir.resolve().relative_to(root.resolve())
    except ValueError:
        return None
    # The root itself would ignore the whole project.
    if relative == Path():
        return None
    return f"/{relative.as_posix()}/"


def add_to_gitignore(root: Path, entry: str) -> bool:
    """Append `entry` to `root/.gitignore` unless it is already there; True if it was added."""
    gitignore = root / ".gitignore"
    # surrogateescape keeps bytes that aren't UTF-8 intact when the file is written back.
    existing = gitignore.read_text(encoding="utf-8", errors="surrogateescape") if gitignore.is_file() else ""
    lines = {line.strip() for line in existing.splitlines()}
    if entry in lines or entry.strip("/") in lines:
        return False
    separator = "" if not existing or existing.endswith("\n") else "\n"
    gitignore.write_text(f"{existing}{separator}{entry}\n", encoding="utf-8", errors="surrogateescape")
    return True

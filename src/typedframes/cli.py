"""CLI entry point for typedframes checker."""

from __future__ import annotations

import argparse
import functools
import json
import os
import re
import sys
import time
import tomllib
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

# ANSI escape sequences
_RESET = "\033[0m"
_BOLD = "\033[1m"
_BOLD_RED = "\033[1;31m"
_BOLD_GREEN = "\033[1;32m"
_BOLD_YELLOW = "\033[1;33m"
_DIM = "\033[2m"  # low-key informational text: coverage summary, non-actionable suggestions

# Directories that should never be descended into when collecting .py files by
# default: VCS metadata, virtualenvs, caches, editor/tool state, and vendored/build
# trees. Mirrors ruff's default exclude list, a well-established precedent for
# "directories no linter should scan" -- including ruff's own override semantics:
# [tool.typedframes] exclude in pyproject.toml REPLACES this set entirely rather than
# adding to it (see _load_configured_excludes/_collect_python_files).
_EXCLUDED_DIRS = frozenset(
    {
        ".bzr",
        ".claude",
        ".direnv",
        ".eggs",
        ".git",
        ".git-rewrite",
        ".hg",
        ".ipynb_checkpoints",
        ".mypy_cache",
        ".nox",
        ".pants.d",
        ".pytest_cache",
        ".pytype",
        ".ruff_cache",
        ".svn",
        ".tox",
        ".venv",
        ".vscode",
        ".idea",
        "__pycache__",
        "_build",
        "buck-out",
        "build",
        "dist",
        "node_modules",
        "site-packages",
        "venv",
    }
)


def _load_configured_excludes(path: Path) -> frozenset[str] | None:
    """Read `[tool.typedframes] exclude` from path/pyproject.toml, if present.

    Returns `None` when nothing is configured (no pyproject.toml, no
    `[tool.typedframes]` section, no `exclude` key, or a malformed value) -- the
    caller's signal to fall back to the built-in default set (`_EXCLUDED_DIRS`)
    rather than pruning nothing. An explicitly empty `exclude = []` is NOT `None`: it
    means the user deliberately wants no directories pruned at all, same as any other
    override value.

    Mirrors the Rust checker's own `[tool.typedframes] exclude` key (see
    `load_linter_config` in rust/src/lib.rs) so a single config value controls both
    collectors, regardless of which one runs for a given invocation. Only looks at
    `path` itself (no walking up the ancestor chain) -- matches `_build_index_bytes`'s
    directory case, which treats `path` as the project root. Nothing to resolve when
    `path` is a single file: it is its own complete file list, so there is no descent
    to prune.
    """
    if not path.is_dir():
        return None
    config_path = path / "pyproject.toml"
    if not config_path.is_file():
        return None
    try:
        with config_path.open("rb") as f:
            data = tomllib.load(f)
    except (tomllib.TOMLDecodeError, OSError):
        return None
    exclude = data.get("tool", {}).get("typedframes", {}).get("exclude")
    if not isinstance(exclude, list):
        return None
    return frozenset(entry for entry in exclude if isinstance(entry, str))


# DataFrame schema coverage enforcement is entirely opt-in: with no
# `[tool.typedframes.coverage]` table (or `[coverage]` in typedframes.toml) and no
# `--coverage-fail-under`, no threshold is ever evaluated and `check` behaves exactly as it did
# before the feature existed -- same output, same exit code. `_COVERAGE_DEFAULT_FAIL_UNDER`
# is therefore the default only once enforcement has been switched on; it is NOT a
# threshold applied to unconfigured projects.
_COVERAGE_DEFAULT_FAIL_UNDER = 100.0
_COVERAGE_PCT_MAX = 100.0

# Coverage detail levels: how much DataFrame schema coverage detail to print, as text
# (or nested as structured JSON under --output-format=json). Deliberately just two
# verbosity levels, not a format -- "summary" is the pre-existing one-line message and
# the default, so an unconfigured project sees exactly what it saw before; JSON-ness is
# --output-format's job alone, not a third value here (see --coverage-detail's help).
# "term-missing" is named after `coverage report -m`.
_COVERAGE_DETAILS = ("summary", "term-missing")


def _coverage_warn(message: str) -> None:
    """Warn about unusable DataFrame schema coverage config on stderr, keeping stdout clean.

    Bad config is reported rather than silently defaulted: a threshold the user
    believes is enforced but isn't is worse than a noisy run.
    """
    print(f"typedframes: {message}", file=sys.stderr)


@dataclass(frozen=True)
class CoverageConfig:
    """Resolved DataFrame schema coverage threshold settings for one `check` invocation.

    The default instance is the "nothing configured" state -- disabled, so no
    threshold is evaluated at all.
    """

    enabled: bool = False
    fail_under: float = _COVERAGE_DEFAULT_FAIL_UNDER
    overrides: tuple[tuple[str, float], ...] = ()

    detail: str = "summary"
    """How much DataFrame schema coverage detail to print.

    Independent of `enabled`, which gates threshold *enforcement* only -- asking
    for a detailed report is not the same as asking for a gate, and either is
    useful without the other.
    """


def _parse_threshold(value: object, label: str) -> float | None:
    """Coerce a configured threshold to a percentage, or warn and return `None`."""
    # bool is a subclass of int, and `fail_under = true` is a mistake, not 100%.
    if isinstance(value, bool) or not isinstance(value, int | float):
        _coverage_warn(f"ignoring {label}: expected a number, got {type(value).__name__}")
        return None
    pct = float(value)
    if not 0.0 <= pct <= _COVERAGE_PCT_MAX:
        _coverage_warn(f"ignoring {label}: {pct} is outside the 0-100 range")
        return None
    return pct


def _read_toml_table(config_path: Path, keys: tuple[str, ...]) -> dict | None:
    """Read a nested table out of a TOML file, or `None` if it isn't there.

    A missing file is silent (nothing was configured); a malformed one warns,
    since the user clearly meant to configure something.
    """
    if not config_path.is_file():
        return None
    try:
        with config_path.open("rb") as f:
            data = tomllib.load(f)
    except (tomllib.TOMLDecodeError, OSError) as e:
        _coverage_warn(f"ignoring {config_path}: {e}")
        return None
    table: object = data
    for key in keys:
        if not isinstance(table, dict):
            return None
        table = table.get(key)
    return table if isinstance(table, dict) else None


def _coverage_config_from_table(table: dict) -> CoverageConfig:
    """Build a `CoverageConfig` from a raw TOML table, dropping unusable values."""
    enabled = table.get("enabled", False)
    if not isinstance(enabled, bool):
        _coverage_warn("ignoring coverage.enabled: expected true or false")
        enabled = False

    fail_under = _COVERAGE_DEFAULT_FAIL_UNDER
    if "fail_under" in table:
        parsed = _parse_threshold(table["fail_under"], "coverage.fail_under")
        if parsed is not None:
            fail_under = parsed

    detail = table.get("detail", "summary")
    if detail not in _COVERAGE_DETAILS:
        _coverage_warn(f"ignoring coverage.detail: expected one of {', '.join(_COVERAGE_DETAILS)}")
        detail = "summary"

    raw_overrides = table.get("overrides", {})
    overrides: list[tuple[str, float]] = []
    if isinstance(raw_overrides, dict):
        for pattern, value in raw_overrides.items():
            parsed = _parse_threshold(value, f"coverage.overrides[{pattern!r}]")
            if parsed is not None:
                overrides.append((pattern, parsed))
    else:
        # `overrides` defaults to an empty dict above, so reaching here means the key
        # was present with a non-table value -- including `overrides = []`, which is a
        # type mistake worth reporting rather than silently treating as "none set".
        _coverage_warn("ignoring coverage.overrides: expected a table of glob = threshold")

    return CoverageConfig(
        enabled=enabled,
        fail_under=fail_under,
        overrides=tuple(overrides),
        detail=detail,
    )


def _load_coverage_config(path: Path) -> CoverageConfig:
    """Read the opt-in DataFrame schema coverage settings for the project at `path`.

    Two sources, with ruff's precedence rule: a standalone `typedframes.toml` at
    the project root wins ENTIRELY over `[tool.typedframes]` in `pyproject.toml`
    -- the two are never merged, so exactly one file explains the whole
    configuration. In `typedframes.toml` the `[tool.typedframes]` prefix is
    dropped, exactly as `ruff.toml` drops `[tool.ruff]`, making the table
    `[coverage]` rather than `[tool.typedframes.coverage]`.

    Only looks at `path`, and only when it is a directory -- no walking up the
    ancestor chain -- matching `_load_configured_excludes` and `_build_index_bytes`'s
    directory case, which treat `path` as the project root. Checking a single file
    therefore never picks up a coverage config file; `--coverage-fail-under` covers
    that case. (Single-file *indexing* does walk up to find a project root, since it
    has to resolve the file's imports against something -- coverage thresholds are a
    deliberate opt-in and are left alone.)

    Returns the disabled default whenever nothing is configured or the config is
    unusable, which is what keeps the opt-in guarantee: no table, no threshold.
    """
    if not path.is_dir():
        return CoverageConfig()

    standalone = path / "typedframes.toml"
    if standalone.is_file():
        table = _read_toml_table(standalone, ("coverage",))
    else:
        table = _read_toml_table(path / "pyproject.toml", ("tool", "typedframes", "coverage"))

    if table is None:
        return CoverageConfig()
    return _coverage_config_from_table(table)


@functools.cache
def _glob_to_regex(pattern: str) -> re.Pattern[str]:
    """Compile a project-relative path glob to an anchored regex.

    `**` spans any number of path segments; `*` and `?` never cross a `/`;
    everything else is literal. Paths are matched in project-relative POSIX form
    (`legacy/etl/load.py`), so `legacy/**` matches every file beneath `legacy/`
    at any depth.

    Hand-rolled because neither available alternative fits: `PurePath.full_match`
    needs Python 3.13 and this package supports 3.11, and `pathspec` would be the
    first runtime dependency in a package that deliberately has none.
    """
    parts: list[str] = []
    i = 0
    while i < len(pattern):
        char = pattern[i]
        if char == "*":
            star_end = i
            while star_end < len(pattern) and pattern[star_end] == "*":
                star_end += 1
            if pattern.startswith("**", i):
                # `**/` consumes the separator too, so `**/x.py` still matches a
                # bare `x.py` at the root rather than requiring a leading directory.
                if star_end < len(pattern) and pattern[star_end] == "/":
                    parts.append("(?:.*/)?")
                    star_end += 1
                else:
                    parts.append(".*")
            else:
                parts.append("[^/]*")
            i = star_end
        elif char == "?":
            parts.append("[^/]")
            i += 1
        else:
            parts.append(re.escape(char))
            i += 1
    return re.compile("".join(parts) + r"\Z")


def _pattern_specificity(pattern: str) -> tuple[int, int, str]:
    """Rank an override pattern for most-specific-match-wins resolution.

    Specificity is the length of the literal prefix before the first glob
    metacharacter -- so `src/new_module/**` (15) beats `src/**` (4) for a file
    under both -- then total pattern length, then the pattern itself so that ties
    resolve deterministically instead of by table iteration order.
    """
    literal_prefix = re.split(r"[*?]", pattern, maxsplit=1)[0]
    return (len(literal_prefix), len(pattern), pattern)


def _collect_files_with_suffix(path: Path, suffix: str, configured_excludes: frozenset[str] | None) -> list[Path]:
    """Collect all files with the given suffix from a path (file or directory).

    Prunes descent into vendored/VCS/cache directories: `configured_excludes` (see
    `_load_configured_excludes`) REPLACES the built-in default set (``_EXCLUDED_DIRS``)
    entirely when given -- it does not add to it, matching ruff's own `exclude`
    semantics (as opposed to `extend-exclude`, which this checker doesn't have a
    separate option for). `None` (the default -- nothing configured) falls back to
    pruning the built-in default set alone.
    """
    if path.is_file():
        if path.suffix == suffix:
            return [path]
        return []

    excluded_dirs = _EXCLUDED_DIRS if configured_excludes is None else configured_excludes
    found = []
    for dirpath, dirnames, filenames in os.walk(path):
        dirnames[:] = [d for d in dirnames if d not in excluded_dirs]
        found.extend(Path(dirpath) / filename for filename in filenames if filename.endswith(suffix))
    return sorted(found)


def _collect_python_files(path: Path, configured_excludes: frozenset[str] | None = None) -> list[Path]:
    """Collect all .py files from a path (file or directory). See `_collect_files_with_suffix`."""
    return _collect_files_with_suffix(path, ".py", configured_excludes)


def _collect_notebook_files(path: Path, configured_excludes: frozenset[str] | None = None) -> list[Path]:
    """Collect all .ipynb files from a path (file or directory). See `_collect_files_with_suffix`."""
    return _collect_files_with_suffix(path, ".ipynb", configured_excludes)


# One file's check outcome: (errors, dataframes_total, dataframes_typed, untyped_sites).
_FileCheckResult = tuple[list[dict], int, int, list[dict]]
_CheckFileFn = Callable[[str, bytes | None], str]


def _check_python_file(file_path: Path, check_file: _CheckFileFn, index_bytes: bytes | None) -> _FileCheckResult | None:
    """Run the Rust checker on one `.py` file, or return `None` if it had to be skipped."""
    try:
        result_json = check_file(str(file_path), index_bytes)
    except OSError as e:
        print(f"{file_path}: skipped, {e}", file=sys.stderr)
        return None
    except RuntimeError as e:
        # check_file_internal's only failure path is a parse_module() error (see
        # rust/src/lib.rs), i.e. this file's source isn't valid Python syntax --
        # not an internal linter bug -- so it's safe to skip and keep going, same
        # as the OSError case above.
        print(f"{file_path}: skipped, {e}", file=sys.stderr)
        return None

    result = json.loads(result_json)
    errors = result["errors"]
    for error in errors:
        error["file"] = str(file_path)
    sites = [{**site, "file": str(file_path)} for site in result["stats"].get("untyped_sites", [])]
    return errors, result["stats"]["dataframes_total"], result["stats"]["dataframes_typed"], sites


def _check_notebook_file(
    file_path: Path, check_notebook: _CheckFileFn, index_bytes: bytes | None
) -> _FileCheckResult | None:
    """Run the Rust checker on one `.ipynb` file.

    Mirrors `_check_python_file`, but calls the `check_notebook` entry point: the
    notebook's code cells are parsed and concatenated entirely in Rust (via
    `ruff_notebook`, the same crate Ruff and Pyrefly use for their own notebook
    linting), so every error/site already carries a `cell` field and cell-relative
    `line` by the time it reaches Python -- no synthetic file, no remapping here.
    """
    try:
        result_json = check_notebook(str(file_path), index_bytes)
    except OSError as e:
        print(f"{file_path}: skipped, {e}", file=sys.stderr)
        return None
    except RuntimeError as e:
        # Covers a genuine syntax error unrelated to magics, malformed notebook
        # JSON, and a non-Python-kernel notebook -- see check_notebook in
        # rust/src/pyapi.rs for exactly which of those map here.
        print(f"{file_path}: skipped, {e}", file=sys.stderr)
        return None

    result = json.loads(result_json)
    errors = result["errors"]
    for error in errors:
        error["file"] = str(file_path)
    sites = [{**site, "file": str(file_path)} for site in result["stats"].get("untyped_sites", [])]
    return errors, result["stats"]["dataframes_total"], result["stats"]["dataframes_typed"], sites


def _check_files(files: list[Path], *, index_bytes: bytes | None = None) -> tuple[list[dict], dict]:
    """Run the Rust checker on each file, dispatching `.ipynb` notebooks through `_check_notebook_file`.

    Returns all errors with file paths attached, plus coverage stats
    (``dataframes_total``/``dataframes_typed``) aggregated across every file checked,
    and a ``per_file`` mapping of each file to its own ``(total, typed)`` tally --
    needed to attribute coverage to per-path threshold overrides, which grade
    subtrees separately rather than judging one project-wide ratio.

    Also returns ``untyped_sites``: every DataFrame origin the checker recognized
    but could not resolve columns for, with the file it came from attached. This
    is the "missing" listing behind ``--coverage-detail=term-missing``, the
    counterpart to `coverage report -m`'s missing line numbers.
    """
    try:
        from typedframes._rust_checker import check_file, check_notebook
    except ImportError:
        msg = (
            "The Rust checker extension was not found. "
            "Ensure typedframes was installed from a wheel or built with: maturin develop"
        )
        print(msg, file=sys.stderr)
        sys.exit(1)

    all_errors: list[dict] = []
    totals = {"dataframes_total": 0, "dataframes_typed": 0}
    per_file: dict[str, tuple[int, int]] = {}
    untyped_sites: list[dict] = []
    for file_path in files:
        if file_path.suffix == ".ipynb":
            outcome = _check_notebook_file(file_path, check_notebook, index_bytes)
        else:
            outcome = _check_python_file(file_path, check_file, index_bytes)
        if outcome is None:
            continue
        errors, file_total, file_typed, sites = outcome
        all_errors.extend(errors)
        per_file[str(file_path)] = (file_total, file_typed)
        totals["dataframes_total"] += file_total
        totals["dataframes_typed"] += file_typed
        untyped_sites.extend(sites)
    return all_errors, {**totals, "per_file": per_file, "untyped_sites": untyped_sites}


def _error_location(error: dict) -> str:
    """Render an error's position: `line:col`, or `cell N:line:col` for a notebook error."""
    cell = error.get("cell")
    prefix = f"cell {cell}:" if cell is not None else ""
    return f"{prefix}{error['line']}:{error['col']}"


def _format_text(errors: list[dict], *, color: bool = False) -> str:
    """Format errors as text lines using ty-style file:line:col: severity[code] message."""
    lines = []
    for error in errors:
        severity = error.get("severity", "error")
        code = error.get("code", "")
        file_ = error["file"]
        location = _error_location(error)
        message = error["message"]
        code_part = f"[{code}]" if code else ""
        if color:
            if severity == "error":
                sev_colored = f"{_BOLD_RED}error{_RESET}"
            elif severity == "warning":
                sev_colored = f"{_BOLD_YELLOW}warning{_RESET}"
            else:
                sev_colored = f"{_DIM}info{_RESET}"
            lines.append(f"{_BOLD}{file_}{_RESET}:{location}: {sev_colored}{code_part} {message}")
        else:
            lines.append(f"{file_}:{location}: {severity}{code_part} {message}")
    return "\n".join(lines)


# GitHub Actions workflow commands only recognize error/warning/notice severities;
# "info" (typedframes' own low-key tier) maps to "notice", GitHub's closest equivalent.
_GITHUB_SEVERITY = {"error": "error", "warning": "warning", "info": "notice"}


def _format_github(errors: list[dict]) -> str:
    """Format errors as GitHub Actions workflow commands.

    GitHub annotations only understand real file line numbers, which a notebook
    doesn't have -- `.ipynb` is JSON, not source text. For a notebook error,
    `line`/`col` stay cell-relative (the position a viewer would need to look
    up inside that cell) and the cell number is folded into the title instead,
    so the annotation is still attributable even though it won't land on the
    exact right raw-JSON line in GitHub's diff view.
    """
    lines = []
    for error in errors:
        severity = error.get("severity", "error")
        gh_severity = _GITHUB_SEVERITY.get(severity, severity)
        code = error.get("code", "")
        file_ = error["file"]
        line = error["line"]
        col = error["col"]
        message = error["message"]
        title = code or severity
        cell = error.get("cell")
        if cell is not None:
            title = f"{title} (notebook cell {cell})"
        lines.append(f"::{gh_severity} file={file_},line={line},col={col},title={title}::{message}")
    return "\n".join(lines)


def _percentage(value: str) -> float:
    """Argparse type for a 0-100 percentage, so bad input exits 2 like any usage error."""
    try:
        pct = float(value)
    except ValueError as e:
        msg = f"invalid percentage: {value!r}"
        raise argparse.ArgumentTypeError(msg) from e
    if not 0.0 <= pct <= _COVERAGE_PCT_MAX:
        msg = f"percentage must be between 0 and 100, got {pct}"
        raise argparse.ArgumentTypeError(msg)
    return pct


def main(argv: list[str] | None = None) -> None:
    """Entry point for the typedframes CLI."""
    parser = argparse.ArgumentParser(prog="typedframes", description="Static analysis for DataFrame column schemas.")
    subparsers = parser.add_subparsers(dest="command")

    check_parser = subparsers.add_parser("check", help="Check Python files for column errors.")
    check_parser.add_argument("path", type=Path, help="File or directory to check.")
    check_parser.add_argument("--strict", action="store_true", help="Exit with code 1 if any errors are found.")
    check_parser.add_argument(
        "--output-format",
        choices=["text", "json", "github"],
        default="text",
        dest="output_format",
        help="Output format: text (default), json, or github (GitHub Actions annotations).",
    )
    # --json kept as a hidden alias for backward compatibility
    check_parser.add_argument(
        "--json",
        dest="output_format",
        action="store_const",
        const="json",
        help=argparse.SUPPRESS,
    )
    check_parser.add_argument(
        "--no-index",
        action="store_true",
        help="Disable the cross-file index entirely; check every file in isolation, following no imports.",
    )
    check_parser.add_argument(
        "--no-warnings",
        action="store_true",
        help="Suppress all warnings (dropped-unknown-column and untracked-dataframe).",
    )
    check_parser.add_argument(
        "--strict-ingest",
        action="store_true",
        help="Deprecated, no-op: untracked-dataframe is a warning-level diagnostic by default now.",
    )
    check_parser.add_argument(
        "--lenient-ingest",
        action="store_true",
        help=(
            "Downgrade untracked-dataframe from a warning-level diagnostic back to "
            "info-level for bare DataFrame loads without usecols= or columns=."
        ),
    )
    check_parser.add_argument(
        "--no-info",
        action="store_true",
        help="Suppress informational output: the DataFrame schema coverage summary and info-level diagnostics.",
    )
    check_parser.add_argument(
        "--coverage-fail-under",
        type=_percentage,
        default=None,
        dest="fail_under",
        metavar="N",
        help=(
            "Enforce a minimum DataFrame schema coverage: exit 1 if fewer than N%% of "
            "DataFrames have recognized column info. A total override: it applies one "
            "threshold everywhere and ignores [tool.typedframes.coverage] entirely, "
            "per-path overrides included."
        ),
    )
    check_parser.add_argument(
        "--coverage-detail",
        choices=list(_COVERAGE_DETAILS),
        default=None,
        dest="coverage_detail",
        help=(
            "How much DataFrame schema coverage detail to print: summary (default, one line) or "
            "term-missing (per-file table plus the DataFrame sites lacking column info). Combine "
            "with --output-format=json to get the same detail as structured JSON, nested under a "
            "'coverage' key, instead of text -- there is no separate json value here, since picking "
            "a format is --output-format's job alone. Overrides the `detail` key in "
            "[tool.typedframes.coverage]."
        ),
    )

    args = parser.parse_args(argv)

    if args.command != "check":
        parser.print_help()
        sys.exit(2)

    _run_check(args)


@dataclass
class RunStats:
    """Timing plus DataFrame schema coverage stats for a single `check` invocation."""

    elapsed: float
    dataframes_total: int
    dataframes_typed: int


def _coverage_message(stats: RunStats) -> str:
    """Build the low-key DataFrame schema coverage summary line.

    Framed as a signal of how much information the checker had, not a validation
    result \u2014 a low ratio means the check had little to validate, not that the
    code is broken. Named in full ("DataFrame schema coverage") on the way out
    because the surrounding CLI vocabulary is borrowed from coverage.py, and a
    bare "coverage" here reads as test coverage to anyone skimming CI output.
    """
    if stats.dataframes_total == 0:
        return "\u2139 No DataFrames with recognized loads/schemas found to check"
    pct = round(100 * stats.dataframes_typed / stats.dataframes_total)
    return (
        f"\u2139 {stats.dataframes_typed}/{stats.dataframes_total} DataFrames had column info "
        f"({pct}%) \u2014 DataFrame schema coverage, not a pass/fail result"
    )


@dataclass(frozen=True)
class CoverageBucket:
    """One threshold and the aggregate DataFrame tally it is judged against."""

    label: str | None
    """The override glob this bucket came from, or `None` for the global threshold."""

    threshold: float
    total: int
    typed: int

    @property
    def pct(self) -> float:
        """Coverage percentage, unrounded so a near miss isn't displayed as a pass."""
        if self.total == 0:
            return _COVERAGE_PCT_MAX
        return _COVERAGE_PCT_MAX * self.typed / self.total


def _relative_posix(file_str: str, root: Path) -> str:
    """Project-relative POSIX form of a checked file, the form globs match against."""
    try:
        return Path(file_str).relative_to(root).as_posix()
    except ValueError:
        return Path(file_str).as_posix()


def _override_for(rel_path: str, config: CoverageConfig) -> tuple[str, float] | None:
    """Pick the most specific override matching `rel_path`, or `None` for the global bucket."""
    matches = [
        (pattern, threshold) for pattern, threshold in config.overrides if _glob_to_regex(pattern).match(rel_path)
    ]
    if not matches:
        return None
    return max(matches, key=lambda item: _pattern_specificity(item[0]))


def _evaluate_coverage(
    per_file: dict[str, tuple[int, int]],
    config: CoverageConfig,
    root: Path,
    cli_fail_under: float | None,
) -> list[CoverageBucket]:
    """Group checked files by the threshold that governs them and return the failures.

    Each file lands in exactly one bucket -- the most specific matching override,
    else the global `fail_under` -- and each bucket is graded on its own
    aggregate ratio, so a legacy subtree held to 50% cannot drag down (or be
    rescued by) the rest of the project.

    `--coverage-fail-under` is a TOTAL override: one threshold for every file, per-path
    overrides ignored, so a one-off `--coverage-fail-under=100` really does mean 100
    everywhere rather than being quietly capped by a legacy exemption in config.

    A bucket with no recognized DataFrames passes vacuously: 0/0 means the
    checker found nothing to measure there, not that the code failed, which is
    the same reading `_coverage_message` already gives an empty run.
    """
    tallies: dict[str | None, tuple[float, int, int]] = {}
    for file_str, (total, typed) in sorted(per_file.items()):
        if cli_fail_under is not None:
            label, threshold = None, cli_fail_under
        else:
            override = _override_for(_relative_posix(file_str, root), config)
            label, threshold = override if override is not None else (None, config.fail_under)
        _, prev_total, prev_typed = tallies.get(label, (threshold, 0, 0))
        tallies[label] = (threshold, prev_total + total, prev_typed + typed)

    failing = []
    for label, (threshold, total, typed) in tallies.items():
        if total == 0:
            continue
        bucket = CoverageBucket(label=label, threshold=threshold, total=total, typed=typed)
        if bucket.pct < threshold:
            failing.append(bucket)

    # Global bucket first, then overrides alphabetically, so output is stable.
    return sorted(failing, key=lambda b: (b.label is not None, b.label or ""))


def _coverage_failure_message(bucket: CoverageBucket) -> str:
    """Explain one failed threshold: what was required, what was measured, and where.

    The percentage is shown to one decimal rather than rounded to a whole number
    like the informational summary line: 99.6% against a `fail_under = 100` gate
    has to read as a failure, not as a baffling "100% is below the required 100%".
    """
    scope = f" for {bucket.label!r}" if bucket.label else ""
    return (
        f"✗ DataFrame schema coverage {bucket.pct:.1f}% is below the required "
        f"{bucket.threshold:.1f}%{scope} "
        f"({bucket.typed}/{bucket.total} DataFrames had column info)"
    )


def _missing_label(site: dict) -> str:
    """Render one coverage 'Missing' entry: `var:line`, or `cell N:var:line` for a notebook site."""
    cell = site.get("cell")
    prefix = f"cell {cell}:" if cell is not None else ""
    return f"{prefix}{site['var']}:{site['line']}"


def _format_term_missing(
    per_file: dict[str, tuple[int, int]],
    untyped_sites: list[dict],
    root: Path,
) -> str:
    """Render the per-file DataFrame schema coverage table plus the sites that cost coverage.

    Modelled on `coverage report -m`: one row per file with its tally and
    percentage, then the specific DataFrame assignments the checker could not
    resolve -- the counterpart to coverage.py's missing line numbers, so the
    report is actionable rather than just a number.

    Files with no recognized DataFrames are omitted: a row of `0/0` says nothing
    about coverage and would bury the files that do matter.
    """
    rows = [(name, total, typed) for name, (total, typed) in sorted(per_file.items()) if total > 0]
    if not rows:
        return "No DataFrames with recognized loads/schemas found to check"

    sites_by_file: dict[str, list[dict]] = {}
    for site in untyped_sites:
        sites_by_file.setdefault(site["file"], []).append(site)

    display = {name: _relative_posix(name, root) for name, _, _ in rows}
    name_width = max(len("Name"), *(len(display[name]) for name, _, _ in rows))
    header = f"{'Name'.ljust(name_width)}  Typed  Total   Cover   Missing"
    lines = [header, "-" * len(header)]

    for name, total, typed in rows:
        pct = round(_COVERAGE_PCT_MAX * typed / total)
        missing = ", ".join(
            _missing_label(site) for site in sorted(sites_by_file.get(name, []), key=lambda s: s["line"])
        )
        lines.append(f"{display[name].ljust(name_width)}  {typed:>5}  {total:>5}  {pct:>5}%   {missing}".rstrip())

    total_all = sum(total for _, total, _ in rows)
    typed_all = sum(typed for _, _, typed in rows)
    pct_all = round(_COVERAGE_PCT_MAX * typed_all / total_all)
    lines.append("-" * len(header))
    lines.append(f"{'TOTAL'.ljust(name_width)}  {typed_all:>5}  {total_all:>5}  {pct_all:>5}%")
    return "\n".join(lines)


def _coverage_json_payload(
    per_file: dict[str, tuple[int, int]],
    untyped_sites: list[dict],
    root: Path,
) -> dict:
    """Build the machine-readable coverage document nested under `--output-format=json`'s `coverage` key.

    Reached when `--coverage-detail=term-missing` (or the `detail` config key) is
    combined with `--output-format=json`. Percentages are left unrounded here,
    unlike the human-facing table: a consumer deciding whether a gate passed
    needs the real ratio, and can round for display itself.
    """
    sites_by_file: dict[str, list[dict]] = {}
    for site in untyped_sites:
        sites_by_file.setdefault(site["file"], []).append(site)

    files = []
    for name, (total, typed) in sorted(per_file.items()):
        files.append(
            {
                "file": _relative_posix(name, root),
                "dataframes_total": total,
                "dataframes_typed": typed,
                "percent": (_COVERAGE_PCT_MAX * typed / total) if total else None,
                "missing": [
                    {
                        "var": site["var"],
                        "line": site["line"],
                        "col": site["col"],
                        **({"cell": site["cell"]} if "cell" in site else {}),
                    }
                    for site in sorted(sites_by_file.get(name, []), key=lambda s: s["line"])
                ],
            }
        )

    total_all = sum(total for total, _ in per_file.values())
    typed_all = sum(typed for _, typed in per_file.values())
    return {
        "dataframes_total": total_all,
        "dataframes_typed": typed_all,
        "percent": (_COVERAGE_PCT_MAX * typed_all / total_all) if total_all else None,
        "files": files,
    }


def _print_coverage_report(stats: dict, root: Path, *, detail: str) -> None:
    """Emit the richer DataFrame schema coverage report requested by `--coverage-detail`.

    `summary` is the default and prints nothing extra -- the existing one-line
    summary in `_print_results` already covers it, and keeping this a no-op is
    what makes the default path byte-for-byte unchanged.

    Only reached for text/GitHub output -- `--coverage-detail` has just the two
    text-shaped values (`summary`, `term-missing`); under `--output-format=json`
    the same detail is nested into the single JSON payload by `_print_json_results`
    instead, so stdout stays one valid document. There's no `json` value here to
    branch on: picking a format is `--output-format`'s job alone.
    """
    if detail == "summary":
        return

    per_file = stats.get("per_file", {})
    untyped_sites = stats.get("untyped_sites", [])
    print()
    print(_format_term_missing(per_file, untyped_sites, root))


def _print_coverage_failures(buckets: list[CoverageBucket], *, output_format: str) -> None:
    """Report failed thresholds without corrupting machine-readable output.

    Text goes to stdout alongside the other results, `github` gets a workflow
    error annotation, and `json` diverts to stderr so stdout stays a single valid
    JSON document. Never suppressed by `--no-info`: a failed gate is a result,
    not the informational coverage line that flag exists to silence.
    """
    for bucket in buckets:
        message = _coverage_failure_message(bucket)
        if output_format == "github":
            print(f"::error title=typedframes DataFrame schema coverage::{message[2:]}")
        elif output_format == "json":
            print(message, file=sys.stderr)
        else:
            use_color = hasattr(sys.stdout, "isatty") and sys.stdout.isatty()
            print(f"{_BOLD_RED}{message}{_RESET}" if use_color else message)


def _print_json_results(all_errors: list[dict], stats: RunStats, coverage_detail: dict | None) -> None:
    """Print the whole run as a single JSON document.

    `coverage_detail`, when given, is nested under a `coverage` key rather than
    printed separately, so stdout stays one valid document. `None` -- no richer
    report requested -- leaves the payload byte-for-byte as it was before
    coverage reporting existed.
    """
    stats_dict = {"dataframes_total": stats.dataframes_total, "dataframes_typed": stats.dataframes_typed}
    payload: dict = {"errors": all_errors, "stats": stats_dict}
    if coverage_detail is not None:
        payload["coverage"] = coverage_detail
    print(json.dumps(payload, indent=2))


def _print_results(
    files: list[Path],
    all_errors: list[dict],
    stats: RunStats,
    *,
    output_format: str,
    show_info: bool = True,
) -> None:
    """Print check results in the text or GitHub-annotation format.

    JSON output is handled separately by `_print_json_results`, which has to
    assemble one document rather than print progressively.
    """
    errors_only = [e for e in all_errors if e.get("severity") not in ("warning", "info")]
    warnings = [e for e in all_errors if e.get("severity") == "warning"]

    use_color = output_format == "text" and hasattr(sys.stdout, "isatty") and sys.stdout.isatty()

    if output_format == "github":
        if all_errors:
            print(_format_github(all_errors))
        if show_info:
            print(f"::notice title=typedframes DataFrame schema coverage::{_coverage_message(stats)[2:]}")
        return

    # text format
    if all_errors:
        print(_format_text(all_errors, color=use_color))
        print()

    file_label = "file" if len(files) == 1 else "files"
    if errors_only or warnings:
        parts = []
        if errors_only:
            error_label = "error" if len(errors_only) == 1 else "errors"
            parts.append(f"{len(errors_only)} {error_label}")
        if warnings:
            warn_label = "warning" if len(warnings) == 1 else "warnings"
            parts.append(f"{len(warnings)} {warn_label}")
        summary = ", ".join(parts)
        msg = f"\u2717 Found {summary} in {len(files)} {file_label} ({stats.elapsed:.1f}s)"
        print(f"{_BOLD_RED}{msg}{_RESET}" if use_color else msg)
    else:
        msg = f"\u2713 Checked {len(files)} {file_label} in {stats.elapsed:.1f}s"
        print(f"{_BOLD_GREEN}{msg}{_RESET}" if use_color else msg)

    if show_info:
        coverage_msg = _coverage_message(stats)
        print(f"{_DIM}{coverage_msg}{_RESET}" if use_color else coverage_msg)


def _apply_diagnostic_policy(all_errors: list[dict], args: argparse.Namespace) -> list[dict]:
    """Apply --lenient-ingest severity downgrade and --no-warnings/--no-info filtering.

    untracked-dataframe is a warning-level diagnostic by default; --lenient-ingest
    downgrades it to an info-level "here's what the checker couldn't see" note for
    users who want it to read as less actionable (e.g. exploratory/EDA work). Rust
    already reports it as "warning" natively, so the default case needs no rewriting
    here -- only --lenient-ingest's explicit opt-out does.
    """
    if args.lenient_ingest:
        for e in all_errors:
            if e.get("code") == "untracked-dataframe":
                e["severity"] = "info"

    if args.no_warnings:
        all_errors = [e for e in all_errors if e.get("severity") != "warning"]
    if args.no_info:
        all_errors = [e for e in all_errors if e.get("severity") != "info"]
    return all_errors


def _build_index_bytes(path: Path, args: argparse.Namespace) -> bytes | None:
    """Build the cross-file index for this run, unless `--no-index` disabled it.

    Two shapes, picked by what `path` is:

    - a directory -- the whole-project index, treating `path` itself as the project
      root: every `.py` file underneath it, plus any external package the project
      calls in a DataFrame-shaped way.
    - a single file -- an index scoped to that file's own references: the file, the
      project-local modules its imports reach (transitively, bounded), and the
      external packages that closure calls. The project root is found by walking up
      from the file to the nearest `pyproject.toml`, so this deliberately does NOT
      treat the file's own directory as the root the way the directory case does.
      `None` comes back when there is no `pyproject.toml` anywhere above the file --
      nothing to resolve against, so the file is checked in isolation.

    `--no-index` remains the explicit opt-out from both: truly no index, every file
    checked in isolation, no import following of any kind.
    """
    if args.no_index:
        return None
    try:
        from typedframes._rust_checker import build_project_index, build_single_file_index
    except ImportError:
        return None
    if path.is_dir():
        return build_project_index(str(path))
    return build_single_file_index(str(path))


def _run_check(args: argparse.Namespace) -> None:
    """Execute the check subcommand."""
    path: Path = args.path.resolve()

    if not path.exists():
        original = args.path
        if original.is_absolute():
            print(f"Error: path does not exist: {path}", file=sys.stderr)
        else:
            print(f"Error: path does not exist: {original!r} (resolved to {path})", file=sys.stderr)
        sys.exit(2)

    # Loaded before the run so an unusable config is reported up front rather than
    # after a long check.
    coverage_config = _load_coverage_config(path)

    index_bytes = _build_index_bytes(path, args)

    configured_excludes = _load_configured_excludes(path)
    files = sorted(
        _collect_python_files(path, configured_excludes) + _collect_notebook_files(path, configured_excludes)
    )
    start = time.perf_counter()
    all_errors, coverage = _check_files(files, index_bytes=index_bytes)
    elapsed = time.perf_counter() - start
    stats = RunStats(
        elapsed=elapsed,
        dataframes_total=coverage["dataframes_total"],
        dataframes_typed=coverage["dataframes_typed"],
    )

    all_errors = _apply_diagnostic_policy(all_errors, args)

    errors_only = [e for e in all_errors if e.get("severity") not in ("warning", "info")]
    # The flag wins over the config key, so a one-off `--coverage-detail` doesn't
    # require editing (or temporarily undoing) project config.
    detail = args.coverage_detail or coverage_config.detail
    coverage_detail_payload = (
        _coverage_json_payload(coverage.get("per_file", {}), coverage.get("untyped_sites", []), path)
        if detail != "summary" and args.output_format == "json"
        else None
    )

    if args.output_format == "json":
        _print_json_results(all_errors, stats, coverage_detail_payload)
    else:
        _print_results(
            files,
            all_errors,
            stats,
            output_format=args.output_format,
            show_info=not args.no_info,
        )
        _print_coverage_report(coverage, path, detail=detail)

    # Coverage is a separate gate from --strict: --strict judges correctness (are
    # there errors?), the threshold judges annotation completeness (how much could
    # the checker see?). Enforcing one has never implied the other, and enabling
    # coverage must not silently change what --strict means.
    failing_buckets: list[CoverageBucket] = []
    if args.fail_under is not None or coverage_config.enabled:
        failing_buckets = _evaluate_coverage(
            coverage.get("per_file", {}),
            coverage_config,
            path,
            args.fail_under,
        )
        if failing_buckets:
            _print_coverage_failures(failing_buckets, output_format=args.output_format)

    if failing_buckets or (args.strict and errors_only):
        sys.exit(1)

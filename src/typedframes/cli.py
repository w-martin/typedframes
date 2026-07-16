"""CLI entry point for typedframes checker."""

from __future__ import annotations

import argparse
import json
import sys
import time
from dataclasses import dataclass
from pathlib import Path

# ANSI escape sequences
_RESET = "\033[0m"
_BOLD = "\033[1m"
_BOLD_RED = "\033[1;31m"
_BOLD_GREEN = "\033[1;32m"
_BOLD_YELLOW = "\033[1;33m"
_DIM = "\033[2m"  # low-key informational text: coverage summary, non-actionable suggestions


def _collect_python_files(path: Path) -> list[Path]:
    """Collect all .py files from a path (file or directory)."""
    if path.is_file():
        if path.suffix == ".py":
            return [path]
        return []
    return sorted(path.rglob("*.py"))


def _check_files(files: list[Path], *, index_bytes: bytes | None = None) -> tuple[list[dict], dict]:
    """Run the Rust checker on each file.

    Returns all errors with file paths attached, plus coverage stats
    (``dataframes_total``/``dataframes_typed``) aggregated across every file checked.
    """
    try:
        from typedframes._rust_checker import check_file  # ty: ignore[unresolved-import]
    except ImportError:
        msg = (
            "The Rust checker extension was not found. "
            "Ensure typedframes was installed from a wheel or built with: maturin develop"
        )
        print(msg, file=sys.stderr)
        sys.exit(1)

    all_errors = []
    stats = {"dataframes_total": 0, "dataframes_typed": 0}
    for file_path in files:
        result_json = check_file(str(file_path), index_bytes)
        result = json.loads(result_json)
        errors = result["errors"]
        for error in errors:
            error["file"] = str(file_path)
        all_errors.extend(errors)
        stats["dataframes_total"] += result["stats"]["dataframes_total"]
        stats["dataframes_typed"] += result["stats"]["dataframes_typed"]
    return all_errors, stats


def _format_text(errors: list[dict], *, color: bool = False) -> str:
    """Format errors as text lines using ty-style file:line:col: severity[code] message."""
    lines = []
    for error in errors:
        severity = error.get("severity", "error")
        code = error.get("code", "")
        file_ = error["file"]
        line = error["line"]
        col = error["col"]
        message = error["message"]
        code_part = f"[{code}]" if code else ""
        if color:
            if severity == "error":
                sev_colored = f"{_BOLD_RED}error{_RESET}"
            elif severity == "warning":
                sev_colored = f"{_BOLD_YELLOW}warning{_RESET}"
            else:
                sev_colored = f"{_DIM}info{_RESET}"
            lines.append(f"{_BOLD}{file_}{_RESET}:{line}:{col}: {sev_colored}{code_part} {message}")
        else:
            lines.append(f"{file_}:{line}:{col}: {severity}{code_part} {message}")
    return "\n".join(lines)


# GitHub Actions workflow commands only recognize error/warning/notice severities;
# "info" (typedframes' own low-key tier) maps to "notice", GitHub's closest equivalent.
_GITHUB_SEVERITY = {"error": "error", "warning": "warning", "info": "notice"}


def _format_github(errors: list[dict]) -> str:
    """Format errors as GitHub Actions workflow commands."""
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
        lines.append(f"::{gh_severity} file={file_},line={line},col={col},title={title}::{message}")
    return "\n".join(lines)


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
    check_parser.add_argument("--no-index", action="store_true", help="Disable cross-file index.")
    check_parser.add_argument(
        "--no-warnings",
        action="store_true",
        help="Suppress all warnings (dropped-unknown-column and any enabled ingestion warnings).",
    )
    check_parser.add_argument(
        "--strict-ingest",
        action="store_true",
        help=(
            "Escalate untracked-dataframe from an info-level diagnostic to a warning "
            "for bare DataFrame loads without usecols= or columns=."
        ),
    )
    check_parser.add_argument(
        "--no-info",
        action="store_true",
        help="Suppress informational output: the DataFrame coverage summary and info-level diagnostics.",
    )

    args = parser.parse_args(argv)

    if args.command != "check":
        parser.print_help()
        sys.exit(2)

    _run_check(args)


@dataclass
class RunStats:
    """Timing plus DataFrame coverage stats for a single `check` invocation."""

    elapsed: float
    dataframes_total: int
    dataframes_typed: int


def _coverage_message(stats: RunStats) -> str:
    """Build the low-key DataFrame coverage summary line.

    Framed as a signal of how much information the checker had, not a validation
    result \u2014 a low ratio means the check had little to validate, not that the
    code is broken.
    """
    if stats.dataframes_total == 0:
        return "\u2139 No DataFrames with recognized loads/schemas found to check"
    pct = round(100 * stats.dataframes_typed / stats.dataframes_total)
    return (
        f"\u2139 {stats.dataframes_typed}/{stats.dataframes_total} DataFrames had column info "
        f"({pct}%) \u2014 coverage, not a pass/fail result"
    )


def _print_results(
    files: list[Path],
    all_errors: list[dict],
    stats: RunStats,
    *,
    output_format: str,
    show_info: bool = True,
) -> None:
    """Print check results in the requested format."""
    errors_only = [e for e in all_errors if e.get("severity") not in ("warning", "info")]
    warnings = [e for e in all_errors if e.get("severity") == "warning"]

    if output_format == "json":
        stats_dict = {"dataframes_total": stats.dataframes_total, "dataframes_typed": stats.dataframes_typed}
        print(json.dumps({"errors": all_errors, "stats": stats_dict}, indent=2))
        return

    use_color = output_format == "text" and hasattr(sys.stdout, "isatty") and sys.stdout.isatty()

    if output_format == "github":
        if all_errors:
            print(_format_github(all_errors))
        if show_info:
            print(f"::notice title=typedframes coverage::{_coverage_message(stats)[2:]}")
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
    """Apply --strict-ingest severity escalation and --no-warnings/--no-info filtering.

    untracked-dataframe is an info-level diagnostic by default (a low-key "here's what
    the checker couldn't see" note); --strict-ingest escalates it to a warning for
    users who want it to read as more actionable.
    """
    escalated_severity = "warning" if args.strict_ingest else "info"
    for e in all_errors:
        if e.get("code") == "untracked-dataframe":
            e["severity"] = escalated_severity

    if args.no_warnings:
        all_errors = [e for e in all_errors if e.get("severity") != "warning"]
    if args.no_info:
        all_errors = [e for e in all_errors if e.get("severity") != "info"]
    return all_errors


def _build_index_bytes(path: Path, args: argparse.Namespace) -> bytes | None:
    """Build the cross-file project index, unless disabled or the path isn't a directory."""
    if not path.is_dir() or args.no_index:
        return None
    try:
        from typedframes._rust_checker import build_project_index  # ty: ignore[unresolved-import]

        return build_project_index(str(path))
    except ImportError:
        return None


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

    index_bytes = _build_index_bytes(path, args)

    files = _collect_python_files(path)
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
    _print_results(
        files,
        all_errors,
        stats,
        output_format=args.output_format,
        show_info=not args.no_info,
    )

    if args.strict and errors_only:
        sys.exit(1)

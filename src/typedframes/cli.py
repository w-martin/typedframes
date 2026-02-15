"""CLI entry point for typedframes checker."""

from __future__ import annotations

import argparse
import json
import sys
import time
from pathlib import Path


def _collect_python_files(path: Path) -> list[Path]:
    """Collect all .py files from a path (file or directory)."""
    if path.is_file():
        if path.suffix == ".py":
            return [path]
        return []
    return sorted(path.rglob("*.py"))


def _check_files(files: list[Path]) -> list[dict]:
    """Run the Rust checker on each file, returning all errors with file paths."""
    try:
        from typedframes_checker._rust_checker import check_file  # ty: ignore[unresolved-import]
    except ImportError:
        msg = (
            "The 'check' command requires the typedframes-checker package "
            "(Rust-based column checker, installed separately like type stubs).\n"
            "Install it with: pip install typedframes-checker"
        )
        print(msg, file=sys.stderr)
        sys.exit(1)

    all_errors = []
    for file_path in files:
        result_json = check_file(str(file_path))
        errors = json.loads(result_json)
        for error in errors:
            error["file"] = str(file_path)
        all_errors.extend(errors)
    return all_errors


def _format_human(errors: list[dict]) -> str:
    """Format errors as human-readable lines."""
    return "\n".join(f"\u2717 {error['file']}:{error['line']} - {error['message']}" for error in errors)


def main(argv: list[str] | None = None) -> None:
    """Entry point for the typedframes CLI."""
    parser = argparse.ArgumentParser(prog="typedframes", description="Static analysis for DataFrame column schemas.")
    subparsers = parser.add_subparsers(dest="command")

    check_parser = subparsers.add_parser("check", help="Check Python files for column errors.")
    check_parser.add_argument("path", type=Path, help="File or directory to check.")
    check_parser.add_argument("--strict", action="store_true", help="Exit with code 1 if any errors are found.")
    check_parser.add_argument("--json", dest="json_output", action="store_true", help="Output results as JSON.")

    args = parser.parse_args(argv)

    if args.command != "check":
        parser.print_help()
        sys.exit(2)

    _run_check(args)


def _run_check(args: argparse.Namespace) -> None:
    """Execute the check subcommand."""
    path: Path = args.path.resolve()

    if not path.exists():
        print(f"Error: path does not exist: {path}", file=sys.stderr)
        sys.exit(2)

    files = _collect_python_files(path)
    start = time.perf_counter()
    errors = _check_files(files)
    elapsed = time.perf_counter() - start

    if args.json_output:
        print(json.dumps(errors, indent=2))
    else:
        if errors:
            print(_format_human(errors))
            print()
        file_label = "file" if len(files) == 1 else "files"
        if errors:
            error_label = "error" if len(errors) == 1 else "errors"
            print(f"\u2717 Found {len(errors)} {error_label} in {len(files)} {file_label} ({elapsed:.1f}s)")
        else:
            print(f"\u2713 Checked {len(files)} {file_label} in {elapsed:.1f}s")

    if args.strict and errors:
        sys.exit(1)

"""Benchmark script comparing type checker and linter performance.

Run with: uv run python benchmarks/benchmark_checkers.py
Update README: uv run python benchmarks/benchmark_checkers.py --update-readme
Skip external codebases: uv run python benchmarks/benchmark_checkers.py --skip-external
Use pre-cloned repo: uv run python benchmarks/benchmark_checkers.py --ge-path /path/to/great_expectations

Compares runtime of:
- ruff check (linter)
- ty check (type checker)
- pyrefly (type checker)
- mypy (type checker)
- pyright (type checker)
- typedframes binary (DataFrame column checker)
- mypy + typedframes plugin

Benchmarks run against two codebases to show how tools scale:
- typedframes src/ (small, ~11 files)
- great_expectations/ (large, ~490 files)

All targets are copied to /tmp before benchmarking to avoid filesystem effects.
"""

import argparse
import datetime
import platform
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path

# Number of timed runs per tool. 10 was too few to characterise high-variance
# tools (pyrefly on the small corpus has been observed at CV > 100%, i.e.
# statistically meaningless) -- see GH issue #10.
RUNS = 20
# Number of warmup runs (discarded)
WARMUP = 3

BENCH_DIR = Path(tempfile.gettempdir()) / "typedframes_bench"

GE_REPO_URL = "https://github.com/great-expectations/great_expectations.git"
# Pinned so every run benchmarks the same corpus. Previously cloned at HEAD on
# every invocation, so a machine benchmarked today could get a meaningfully
# different (larger/smaller, differently-structured) codebase than the one
# that produced the committed README numbers -- part of why the v0.2.1->v0.2.2
# README jump (285ms -> 1.79s) turned out not to correspond to any code change
# at all. Bump deliberately when GE ships a new release, not silently.
GE_COMMIT = "1.9.3"


@dataclass
class ToolInfo:
    """Information about a benchmarked tool."""

    name: str
    cmd: list[str]
    description: str
    needs_cache_clear: bool = False
    version: str = ""


@dataclass
class BenchmarkResult:
    """Result of a single benchmark run."""

    name: str
    description: str
    version: str
    times: list[int] = field(default_factory=list)
    success: bool = True
    error: str | None = None

    @property
    def mean(self) -> float:
        """Mean execution time in nanoseconds."""
        return statistics.mean(self.times) if self.times else 0.0

    @property
    def std(self) -> float:
        """Standard deviation in nanoseconds."""
        return statistics.stdev(self.times) if len(self.times) > 1 else 0.0


def copy_to_tmp(source: Path, label: str) -> Path:
    """Copy a directory to /tmp for benchmarking."""
    dest = BENCH_DIR / label
    if dest.exists():
        shutil.rmtree(dest)
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copytree(source, dest)
    return dest


def _find_ge_source(base: Path) -> Path | None:
    """Find the great_expectations source directory within a base path."""
    ge_src = base / "great_expectations"
    return ge_src if ge_src.exists() else None


def clone_great_expectations(ge_path: str | None) -> Path | None:
    """Clone or reuse the Great Expectations repository."""
    if ge_path:
        src = Path(ge_path)
        if not src.exists():
            print(f"Error: --ge-path {ge_path} does not exist")
            return None
        ge_src = src / "great_expectations" if (src / "great_expectations").exists() else src
        return copy_to_tmp(ge_src, "great_expectations")

    clone_dir = Path(tempfile.mkdtemp(prefix="typedframes_ge_"))
    try:
        print(f"Cloning Great Expectations @ {GE_COMMIT} (shallow)...", flush=True)
        result = subprocess.run(
            ["git", "clone", "--depth", "1", "--branch", GE_COMMIT, GE_REPO_URL, str(clone_dir)],
            capture_output=True,
            text=True,
            timeout=120,
            check=False,
        )
        if result.returncode != 0:
            print(f"Failed to clone: {result.stderr.strip()}")
            return None

        ge_src = _find_ge_source(clone_dir)
        if ge_src:
            return copy_to_tmp(ge_src, "great_expectations")
        return None
    finally:
        shutil.rmtree(clone_dir, ignore_errors=True)


def get_tool_version(cmd: list[str], tool_name_override: str | None = None) -> str:
    """Get version string for a tool."""
    version_cmds = {
        "ruff": ["uv", "run", "ruff", "--version"],
        "ty": ["uv", "run", "ty", "--version"],
        "mypy": ["uv", "run", "mypy", "--version"],
        "pyright": ["uv", "run", "pyright", "--version"],
        "pyrefly": ["uv", "run", "pyrefly", "--version"],
        "typedframes": None,  # No --version flag, use package version
    }

    # Find the tool name from command
    tool_name = tool_name_override
    if not tool_name:
        for name in version_cmds:
            if name in " ".join(cmd):
                tool_name = name
                break

    if not tool_name:
        return "unknown"

    # Special case for typedframes binary - get from package
    if tool_name == "typedframes":
        try:
            result = subprocess.run(
                ["uv", "run", "python", "-c", "import typedframes; print(typedframes.__version__)"],
                capture_output=True,
                text=True,
                timeout=10,
                check=False,
            )
            version = result.stdout.strip()
            return version or "0.1.2"
        except (subprocess.TimeoutExpired, FileNotFoundError):
            return "0.1.2"

    try:
        result = subprocess.run(
            version_cmds[tool_name],
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
        version_output = result.stdout.strip() or result.stderr.strip()
        # Extract version number (first line, clean up)
        first_line = version_output.split("\n")[0]
        # Try to extract just the version number
        parts = first_line.split()
        for part in parts:
            if part[0].isdigit():
                return part
        return first_line
    except (subprocess.TimeoutExpired, FileNotFoundError):
        return "unknown"


def _get_ram_gb() -> str:
    """Best-effort total RAM in GiB, without adding a dependency. 'unknown' if undeterminable."""
    try:
        if sys.platform == "darwin":
            result = subprocess.run(
                ["sysctl", "-n", "hw.memsize"], capture_output=True, text=True, timeout=5, check=False
            )
            return f"{int(result.stdout.strip()) / (1024**3):.0f}GiB"
        if sys.platform.startswith("linux"):
            meminfo = Path("/proc/meminfo").read_text()
            for line in meminfo.splitlines():
                if line.startswith("MemTotal:"):
                    kb = int(line.split()[1])
                    return f"{kb / (1024**2):.0f}GiB"
    except (OSError, ValueError, subprocess.TimeoutExpired):
        pass
    return "unknown"


def get_machine_info() -> str:
    """One-line machine spec + date.

    So benchmark numbers are only ever compared to other numbers recorded under a
    documented environment (see GH issue #10: the v0.2.1->v0.2.2 README jump had
    no corresponding code change, and lacking this context made it impossible to
    rule out "different machine" after the fact).
    """
    date = datetime.datetime.now(tz=datetime.UTC).date().isoformat()
    cpu = platform.processor() or platform.machine()
    py = f"{platform.python_implementation()} {platform.python_version()}"
    return f"{date} · {platform.system()} {platform.release()} · {cpu} · {py} · {_get_ram_gb()} RAM"


def clear_caches(target_dir: Path) -> None:
    """Clear type checker caches to ensure fair comparison.

    Notes on cache locations:
    - mypy: writes .mypy_cache/ in the project directory (cleared here)
    - pyright: writes its analysis cache to ~/.cache/pyright/ on macOS/Linux,
      NOT to .pyright/ in the project — both are cleared for safety
    - ruff: bypassed via --no-cache flag on the ruff invocation (no path to clear)
    - ty: writes .ty/ in the project directory; ~/.cache/ty/ cleared for safety
      in case ty uses a platform cache dir in the installed version
    """
    cache_dirs = [
        target_dir / ".mypy_cache",
        # pyright project-level dir (usually empty, but cleared for safety)
        target_dir / ".pyright",
        # pyright user-level analysis cache (the real cache on macOS/Linux)
        Path.home() / ".cache" / "pyright",
        target_dir / ".ty",
        # ty user-level cache (belt-and-suspenders in case version uses platform dir)
        Path.home() / ".cache" / "ty",
    ]
    for cache_dir in cache_dirs:
        if cache_dir.exists():
            shutil.rmtree(cache_dir)


def run_benchmark(
    tool: ToolInfo,
    target: str,
    runs: int = RUNS,
    warmup: int = WARMUP,
    *,
    clear_cache_func: Callable[[], None] | None = None,
) -> BenchmarkResult:
    """Run a benchmark multiple times and collect timing data."""
    full_cmd = [*tool.cmd, target]

    # Check if tool is available
    try:
        check_cmd = tool.cmd[:1] if tool.cmd[0] != "uv" else tool.cmd[:3]
        if tool.cmd[0] == "npx":
            check_cmd = tool.cmd[:2]
        subprocess.run(check_cmd, capture_output=True, timeout=30, check=False)
    except (FileNotFoundError, subprocess.TimeoutExpired):
        return BenchmarkResult(
            name=tool.name,
            description=tool.description,
            version="",
            success=False,
            error="Tool not found",
        )

    # Get version
    # Determine version tool name (handle "mypy + typedframes" -> "mypy")
    version_tool = tool.name.split()[0] if " " in tool.name else tool.name
    version = get_tool_version(tool.cmd, version_tool)

    # Warmup runs (results discarded). For tools we don't cache-clear, this warms
    # up the OS page cache / tool-internal caches as intended. For tools we DO
    # cache-clear before every timed run, warming up here would be pointless (the
    # clear before each timed run wipes it right back out) -- so skip warmup for
    # those entirely rather than burn time clearing-then-immediately-discarding a
    # cache on every warmup iteration too (see GH issue #10). Every timed run for
    # a cache-clearing tool is therefore consistently cold (OS-cold, tool-cold);
    # tools without needs_cache_clear measure whatever their own natural warm
    # state is after `warmup` prior invocations.
    if not clear_cache_func:
        for _ in range(warmup):
            subprocess.run(full_cmd, capture_output=True, check=False)

    # Timed runs - clear cache before each to ensure fair comparison
    times: list[int] = []
    for _ in range(runs):
        if clear_cache_func:
            clear_cache_func()
        start = time.perf_counter_ns()
        subprocess.run(full_cmd, capture_output=True, check=False)
        elapsed = time.perf_counter_ns() - start
        times.append(elapsed)

    return BenchmarkResult(
        name=tool.name,
        description=tool.description,
        version=version,
        times=times,
        success=True,
    )


def format_time(ns: float) -> str:
    """Format time in appropriate units from nanoseconds."""
    if ns < 1_000:
        return f"{ns:.0f}ns"
    if ns < 1_000_000:
        return f"{ns / 1_000:.0f}\u00b5s"
    if ns < 1_000_000_000:
        return f"{ns / 1_000_000:.0f}ms"
    return f"{ns / 1_000_000_000:.2f}s"


def _format_cell(result: BenchmarkResult | None) -> str:
    """Format a single time cell for the markdown table."""
    if result is None:
        return "-"
    if not result.success:
        return result.error or "N/A"
    return f"{format_time(result.mean)} \u00b1{format_time(result.std)}"


def generate_markdown_table(
    tool_results: dict[str, dict[str, BenchmarkResult | None]],
    tool_meta: dict[str, tuple[str, str]],
    codebase_labels: list[tuple[str, int]],
) -> str:
    """Generate markdown table with one column per codebase.

    Args:
        tool_results: {tool_name: {codebase_label: result_or_none}}.
        tool_meta: {tool_name: (version, description)}.
        codebase_labels: [(label, file_count), ...] in column order.
    """
    col_headers = [f"{label} ({count} files)" for label, count in codebase_labels]
    header = "| Tool | Version | What it does | " + " | ".join(col_headers) + " |"
    sep_parts = "|------|---------|--------------|" + "|".join("-" * (len(h) + 2) for h in col_headers) + "|"

    lines = [
        f"**Benchmark results** ({RUNS} runs, {WARMUP} warmup, caches cleared between runs):",
        f"*{get_machine_info()} · Great Expectations pinned @ {GE_COMMIT}*",
        "",
        header,
        sep_parts,
    ]
    for tool_name, per_codebase in tool_results.items():
        results = [per_codebase.get(label) for label, _ in codebase_labels]
        if all(r is not None and not r.success for r in results):
            continue
        version, description = tool_meta[tool_name]
        cells = [_format_cell(r) for r in results]
        lines.append(f"| {tool_name} | {version} | {description} | " + " | ".join(cells) + " |")

    lines.append("")
    lines.append("*Run `uv run python benchmarks/benchmark_checkers.py` to reproduce.*")
    return "\n".join(lines)


def update_readme(
    project_root: Path,
    tool_results: dict[str, dict[str, BenchmarkResult | None]],
    tool_meta: dict[str, tuple[str, str]],
    codebase_labels: list[tuple[str, int]],
) -> None:
    """Update the benchmark table in README.md."""
    readme_path = project_root / "README.md"
    content = readme_path.read_text()

    # Match old or new table format. The machine-info/GE-pin caption line
    # (`*...*`) directly under "**Benchmark results**" is optional so this still
    # matches tables generated before that line existed.
    pattern = (
        r"\*\*Benchmark results\*\*[^\n]*\n"
        r"(?:\*[^\n]*\*\n)?"
        r"\n"
        r"\|[^\n]*\n"
        r"\|[-| ]+\n"
        r"(?:\|[^\n]*\n)*"
        r"\n"
        r"(?:\*\*Note:\*\*[^\n]*(?:\n[^\n*][^\n]*)?\n\n)?"
        r"\*Run `uv run python benchmarks/benchmark_checkers\.py` to reproduce\.\*"
    )

    new_table = generate_markdown_table(tool_results, tool_meta, codebase_labels)

    # Lambda replacement so literal backslashes in new_table (unlikely, but e.g. a
    # stray `\d` in a version string) are never misread as regex backreferences.
    new_content, count = re.subn(pattern, lambda _match: new_table, content)

    if count == 0:
        print("Warning: Could not find benchmark table pattern in README.md")
        print("Generated table:\n")
        print(new_table)
        return

    readme_path.write_text(new_content)
    print(f"Updated benchmark table in {readme_path}")


def count_python_files(directory: Path) -> int:
    """Count Python files in directory."""
    return len(list(directory.rglob("*.py")))


def run_codebase_benchmarks(
    label: str,
    target: str,
    target_dir: Path,
    tools: list[ToolInfo],
) -> list[BenchmarkResult]:
    """Run benchmarks for a single codebase."""
    print(f"\n{'=' * 60}")
    print(f"Benchmarking: {label} ({target})")
    print(f"{'=' * 60}")

    results: list[BenchmarkResult] = []

    def clear_cache() -> None:
        clear_caches(target_dir)

    for tool in tools:
        print(f"  Running {tool.name}...", end=" ", flush=True)

        result = run_benchmark(
            tool,
            target,
            clear_cache_func=clear_cache if tool.needs_cache_clear else None,
        )
        results.append(result)

        if result.success:
            print(f"{format_time(result.mean)} (\u00b1{format_time(result.std)}) [{result.version}]")
        else:
            print(f"SKIPPED: {result.error}")

    return results


def _create_mypy_config(*, with_plugin: bool = False) -> Path:
    """Create a minimal mypy config for fair benchmarking."""
    config_path = BENCH_DIR / ("mypy_plugin.ini" if with_plugin else "mypy_vanilla.ini")
    config_path.parent.mkdir(parents=True, exist_ok=True)
    lines = ["[mypy]", "ignore_missing_imports = True", "no_incremental = True"]
    if with_plugin:
        lines.append("plugins = typedframes.mypy")
    config_path.write_text("\n".join(lines) + "\n")
    return config_path


def build_tools() -> list[ToolInfo]:
    """Build the list of tools to benchmark."""
    vanilla_cfg = _create_mypy_config()
    plugin_cfg = _create_mypy_config(with_plugin=True)

    tools: list[ToolInfo] = []
    # Use the Python CLI (uv run typedframes check <target>) so that both files
    # and directories are handled correctly.  The old approach invoked the raw
    # Rust binary, which only accepts a single file, and the benchmark had to
    # special-case a hardcoded example file — invalidating multi-file results.
    tools.append(ToolInfo("typedframes", ["uv", "run", "typedframes", "check"], "DataFrame column checker"))
    tools.extend(
        [
            # --no-cache bypasses ruff's cache (~/.cache/ruff/) so every run is cold;
            # this is more reliable than trying to clear a path that may not exist.
            ToolInfo("ruff", ["uv", "run", "ruff", "check", "--no-cache"], "Linter (no type checking)"),
            ToolInfo("ty", ["uv", "run", "ty", "check"], "Type checker", needs_cache_clear=True),
            ToolInfo("pyrefly", ["uv", "run", "pyrefly", "check"], "Type checker", needs_cache_clear=True),
            ToolInfo(
                "mypy",
                ["uv", "run", "mypy", "--config-file", str(vanilla_cfg)],
                "Type checker (no plugin)",
                needs_cache_clear=True,
            ),
            ToolInfo(
                "mypy + typedframes",
                ["uv", "run", "mypy", "--config-file", str(plugin_cfg)],
                "Type checker + column checker",
                needs_cache_clear=True,
            ),
            ToolInfo("pyright", ["uv", "run", "pyright"], "Type checker", needs_cache_clear=True),
        ]
    )
    return tools


def print_summary(
    tool_results: dict[str, dict[str, BenchmarkResult | None]],
    tool_meta: dict[str, tuple[str, str]],
    codebase_labels: list[tuple[str, int]],
) -> None:
    """Print human-readable summary table to stdout."""
    print("\n" + "=" * 100)
    print("BENCHMARK RESULTS")
    print("=" * 100)

    col_headers = [f"{label} ({count})" for label, count in codebase_labels]
    header = f"{'Tool':<20} {'Version':<10} {'What it does':<28}"
    for h in col_headers:
        header += f" {h:>20}"
    print(header)
    print("-" * 100)

    for tool_name, per_codebase in tool_results.items():
        version, description = tool_meta[tool_name]
        row = f"{tool_name:<20} {version:<10} {description:<28}"
        for label, _ in codebase_labels:
            result = per_codebase.get(label)
            cell = _format_cell(result) if result else "-"
            row += f" {cell:>20}"
        print(row)

    print("\n" + "=" * 100)
    print("MARKDOWN TABLE")
    print("=" * 100)
    print(generate_markdown_table(tool_results, tool_meta, codebase_labels))


def ensure_release_build(project_root: Path) -> None:
    """Rebuild the typedframes Rust extension in release mode before benchmarking.

    `maturin develop` (the documented dev workflow in DEVELOPING.md, also what
    `inv build` runs) builds a DEBUG binary by default -- unoptimized, no LTO, no
    dead-code stripping. Debug and release builds of this checker have been
    measured 5-10x apart on a realistic corpus. Benchmarking whatever happens to
    already be installed silently measures debug performance if a contributor's
    last local build was a plain `maturin develop` -- which is indistinguishable
    from a real regression when comparing against a prior README table. Always
    rebuild in release mode here unless explicitly skipped.
    """
    print("Building typedframes Rust extension in release mode...", flush=True)
    result = subprocess.run(
        ["uv", "run", "maturin", "develop", "--release", "--manifest-path", str(project_root / "rust" / "Cargo.toml")],
        cwd=project_root,
        capture_output=True,
        text=True,
        timeout=600,
        check=False,
    )
    if result.returncode != 0:
        print("Warning: release build failed, benchmarking whatever is currently installed:")
        print(result.stderr.strip())
    else:
        print("Release build OK.")


def main() -> None:
    """Run all benchmarks and print results."""
    parser = argparse.ArgumentParser(description="Benchmark type checkers")
    parser.add_argument("--update-readme", action="store_true", help="Update the benchmark table in README.md")
    parser.add_argument("--skip-external", action="store_true", help="Skip external codebase (Great Expectations)")
    parser.add_argument("--ge-path", type=str, default=None, help="Path to pre-cloned Great Expectations repo")
    parser.add_argument(
        "--skip-build",
        action="store_true",
        help="Skip rebuilding the typedframes extension in release mode; benchmark whatever is installed as-is.",
    )
    args = parser.parse_args()

    project_root = Path(__file__).parent.parent

    if not args.skip_build:
        ensure_release_build(project_root)

    tools = build_tools()

    # Copy typedframes src/ to /tmp
    tf_tmp = copy_to_tmp(project_root / "src", "typedframes-src")
    tf_file_count = count_python_files(tf_tmp)
    tf_label = "typedframes"

    print(f"Copied typedframes src/ to {tf_tmp}")
    print(f"Python files: {tf_file_count}")
    print(f"Runs per tool: {RUNS} (plus {WARMUP} warmup)")
    print("Clearing caches between runs for fair comparison")

    # Run typedframes benchmarks
    tf_results = run_codebase_benchmarks(tf_label, str(tf_tmp), tf_tmp, tools)

    # Collect per-tool, per-codebase results
    tool_results: dict[str, dict[str, BenchmarkResult | None]] = {}
    tool_meta: dict[str, tuple[str, str]] = {}
    codebase_labels: list[tuple[str, int]] = [(tf_label, tf_file_count)]

    for result in tf_results:
        tool_results.setdefault(result.name, {})[tf_label] = result
        tool_meta[result.name] = (result.version, result.description)

    # Run Great Expectations benchmarks
    if not args.skip_external:
        ge_dir = clone_great_expectations(args.ge_path)
        if ge_dir:
            ge_file_count = count_python_files(ge_dir)
            ge_label = "great_expectations"
            print(f"\nGreat Expectations: {ge_file_count} Python files")
            ge_results = run_codebase_benchmarks(ge_label, str(ge_dir), ge_dir, tools)
            codebase_labels.append((ge_label, ge_file_count))
            for result in ge_results:
                tool_results.setdefault(result.name, {})[ge_label] = result
        else:
            print("\nSkipping Great Expectations benchmarks (clone failed or not available)")

    print_summary(tool_results, tool_meta, codebase_labels)

    if args.update_readme:
        update_readme(project_root, tool_results, tool_meta, codebase_labels)


if __name__ == "__main__":
    main()

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
import re
import shutil
import statistics
import subprocess
import tempfile
import time
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path

# Number of timed runs per tool
RUNS = 10
# Number of warmup runs (discarded)
WARMUP = 3

BENCH_DIR = Path(tempfile.gettempdir()) / "typedframes_bench"

GE_REPO_URL = "https://github.com/great-expectations/great_expectations.git"


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

    clone_dir = BENCH_DIR / "great-expectations-repo"
    if not (clone_dir / ".git").exists():
        print("Cloning Great Expectations (shallow)...", flush=True)
        clone_dir.mkdir(parents=True, exist_ok=True)
        result = subprocess.run(
            ["git", "clone", "--depth", "1", GE_REPO_URL, str(clone_dir)],
            capture_output=True,
            text=True,
            timeout=120,
            check=False,
        )
        if result.returncode != 0:
            print(f"Failed to clone: {result.stderr.strip()}")
            return None
    else:
        print("Reusing existing Great Expectations clone")

    ge_src = _find_ge_source(clone_dir)
    if ge_src:
        return copy_to_tmp(ge_src, "great_expectations")
    return None


def get_tool_version(cmd: list[str], tool_name_override: str | None = None) -> str:
    """Get version string for a tool."""
    version_cmds = {
        "ruff": ["uv", "run", "ruff", "--version"],
        "ty": ["uv", "run", "ty", "--version"],
        "mypy": ["uv", "run", "mypy", "--version"],
        "pyright": ["npx", "pyright", "--version"],
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
            return version or "0.1.0"
        except (subprocess.TimeoutExpired, FileNotFoundError):
            return "0.1.0"

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


def clear_caches(target_dir: Path) -> None:
    """Clear type checker caches to ensure fair comparison."""
    cache_dirs = [
        target_dir / ".mypy_cache",
        target_dir / ".pyright",
        target_dir / ".ruff_cache",
        target_dir / ".ty",
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

    # Warmup runs (results discarded, but warms up any internal caches)
    for _ in range(warmup):
        if clear_cache_func:
            clear_cache_func()
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
        "",
        header,
        sep_parts,
    ]
    for tool_name, per_codebase in tool_results.items():
        version, description = tool_meta[tool_name]
        cells = [_format_cell(per_codebase.get(label)) for label, _ in codebase_labels]
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

    # Match old or new table format
    pattern = (
        r"\*\*Benchmark results\*\*[^\n]*\n\n"
        r"\|[^\n]*\n"
        r"\|[-| ]+\n"
        r"(?:\|[^\n]*\n)*"
        r"\n"
        r"(?:\*\*Note:\*\*[^\n]*(?:\n[^\n*][^\n]*)?\n\n)?"
        r"\*Run `uv run python benchmarks/benchmark_checkers\.py` to reproduce\.\*"
    )

    new_table = generate_markdown_table(tool_results, tool_meta, codebase_labels)

    new_content, count = re.subn(pattern, new_table, content)

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
        project_root: Path,
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

        # Use example file for typedframes binary
        if "typedframes_checker" in str(tool.cmd[0]):
            bench_target = str(project_root / "examples" / "typedframes_example.py")
        else:
            bench_target = target

        result = run_benchmark(
            tool,
            bench_target,
            clear_cache_func=clear_cache if tool.needs_cache_clear else None,
        )
        results.append(result)

        if result.success:
            print(f"{format_time(result.mean)} (\u00b1{format_time(result.std)}) [{result.version}]")
        else:
            print(f"SKIPPED: {result.error}")

    return results


def find_binary(project_root: Path) -> Path:
    """Find the typedframes checker binary (release or debug)."""
    binary_path = (
            project_root / "typedframes-checker" / "rust_typedframes_checker" / "target" / "release" / "typedframes_checker"
    )
    if not binary_path.exists():
        binary_path = (
                project_root
                / "typedframes-checker"
                / "rust_typedframes_checker"
                / "target"
                / "debug"
                / "typedframes_checker"
        )
    return binary_path


def _create_mypy_config(*, with_plugin: bool = False) -> Path:
    """Create a minimal mypy config for fair benchmarking."""
    config_path = BENCH_DIR / ("mypy_plugin.ini" if with_plugin else "mypy_vanilla.ini")
    config_path.parent.mkdir(parents=True, exist_ok=True)
    lines = ["[mypy]", "ignore_missing_imports = True", "no_incremental = True"]
    if with_plugin:
        lines.append("plugins = typedframes_checker.mypy")
    config_path.write_text("\n".join(lines) + "\n")
    return config_path


def build_tools(binary_path: Path) -> list[ToolInfo]:
    """Build the list of tools to benchmark."""
    vanilla_cfg = _create_mypy_config()
    plugin_cfg = _create_mypy_config(with_plugin=True)

    tools: list[ToolInfo] = []
    if binary_path.exists():
        tools.append(ToolInfo("typedframes", [str(binary_path)], "DataFrame column checker"))
    tools.extend(
        [
            ToolInfo("ruff", ["uv", "run", "ruff", "check"], "Linter (no type checking)"),
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
            ToolInfo("pyright", ["npx", "pyright"], "Type checker", needs_cache_clear=True),
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


def main() -> None:
    """Run all benchmarks and print results."""
    parser = argparse.ArgumentParser(description="Benchmark type checkers")
    parser.add_argument("--update-readme", action="store_true", help="Update the benchmark table in README.md")
    parser.add_argument("--skip-external", action="store_true", help="Skip external codebase (Great Expectations)")
    parser.add_argument("--ge-path", type=str, default=None, help="Path to pre-cloned Great Expectations repo")
    args = parser.parse_args()

    project_root = Path(__file__).parent.parent
    binary_path = find_binary(project_root)
    tools = build_tools(binary_path)

    # Copy typedframes src/ to /tmp
    tf_tmp = copy_to_tmp(project_root / "src", "typedframes-src")
    tf_file_count = count_python_files(tf_tmp)
    tf_label = "typedframes"

    print(f"Copied typedframes src/ to {tf_tmp}")
    print(f"Python files: {tf_file_count}")
    print(f"Runs per tool: {RUNS} (plus {WARMUP} warmup)")
    print("Clearing caches between runs for fair comparison")

    # Run typedframes benchmarks
    tf_results = run_codebase_benchmarks(tf_label, str(tf_tmp), tf_tmp, tools, project_root)

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
            ge_results = run_codebase_benchmarks(ge_label, str(ge_dir), ge_dir, tools, project_root)
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

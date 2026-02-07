"""Benchmark script comparing type checker and linter performance.

Run with: uv run python benchmarks/benchmark_checkers.py
Update README: uv run python benchmarks/benchmark_checkers.py --update-readme

Compares runtime of:
- ruff check (linter)
- ty check (type checker)
- pyrefly (type checker)
- mypy (type checker)
- pyright (type checker)
- typedframes binary (DataFrame column checker)

Note: On small codebases, startup time dominates. Results may differ significantly
on larger projects where per-file analysis time matters more.
"""

import argparse
import re
import shutil
import statistics
import subprocess
import time
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path

# Number of timed runs per tool
RUNS = 5
# Number of warmup runs (discarded)
WARMUP = 2


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
    times: list[float] = field(default_factory=list)
    success: bool = True
    error: str | None = None

    @property
    def mean(self) -> float:
        """Mean execution time."""
        return statistics.mean(self.times) if self.times else 0.0

    @property
    def std(self) -> float:
        """Standard deviation."""
        return statistics.stdev(self.times) if len(self.times) > 1 else 0.0


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
            return version if version else "0.1.0"
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


def clear_caches(project_root: Path) -> None:
    """Clear type checker caches to ensure fair comparison."""
    cache_dirs = [
        project_root / ".mypy_cache",
        project_root / ".pyright",
        project_root / ".ruff_cache",
        project_root / ".ty",
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
    times: list[float] = []
    for _ in range(runs):
        if clear_cache_func:
            clear_cache_func()
        start = time.perf_counter()
        subprocess.run(full_cmd, capture_output=True, check=False)
        elapsed = time.perf_counter() - start
        times.append(elapsed)

    return BenchmarkResult(
        name=tool.name,
        description=tool.description,
        version=version,
        times=times,
        success=True,
    )


def format_time(seconds: float) -> str:
    """Format time in appropriate units."""
    if seconds < 0.001:
        return f"{seconds * 1000000:.0f}µs"
    if seconds < 1:
        return f"{seconds * 1000:.0f}ms"
    return f"{seconds:.2f}s"


def generate_markdown_table(results: list[BenchmarkResult], file_count: int) -> str:
    """Generate markdown table for README."""
    lines = [
        f"**Benchmark results** ({file_count} Python files, {RUNS} runs each, caches cleared):",
        "",
        "| Tool | Version | What it does | Time |",
        "|------|---------|--------------|------|",
    ]
    for result in results:
        if result.success:
            time_str = f"{format_time(result.mean)} ±{format_time(result.std)}"
            lines.append(f"| {result.name} | {result.version} | {result.description} | {time_str} |")
        else:
            lines.append(f"| {result.name} | - | {result.description} | {result.error} |")

    lines.append("")
    lines.append(
        "**Note:** On small codebases, startup time dominates. "
        "On larger projects, ty and pyrefly are typically 10-60x faster than mypy/pyright."
    )
    lines.append("")
    lines.append("*Run `uv run python benchmarks/benchmark_checkers.py` to reproduce.*")
    return "\n".join(lines)


def update_readme(project_root: Path, results: list[BenchmarkResult], file_count: int) -> None:
    """Update the benchmark table in README.md."""
    readme_path = project_root / "README.md"
    content = readme_path.read_text()

    # Pattern to match the benchmark section (more flexible)
    pattern = (
        r"\*\*Benchmark results\*\*[^\n]*\n\n"
        r"\| Tool \|[^\n]*\n"
        r"\|[-|]+\n"
        r"(?:\|[^\n]*\n)*"
        r"\n"
        r"(?:\*\*Note:\*\*[^\n]*\n\n)?"
        r"\*Run `uv run python benchmarks/benchmark_checkers\.py` to reproduce\.\*"
    )

    new_table = generate_markdown_table(results, file_count)

    # Try to replace existing table
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


def main() -> None:
    """Run all benchmarks and print results."""
    parser = argparse.ArgumentParser(description="Benchmark type checkers")
    parser.add_argument("--update-readme", action="store_true", help="Update the benchmark table in README.md")
    args = parser.parse_args()

    project_root = Path(__file__).parent.parent
    target = str(project_root / "src")
    file_count = count_python_files(project_root / "src")

    # Check for typedframes binary
    binary_path = project_root / "rust_typedframes_linter" / "target" / "release" / "typedframes_linter"
    if not binary_path.exists():
        binary_path = project_root / "rust_typedframes_linter" / "target" / "debug" / "typedframes_linter"

    print(f"Benchmarking type checkers on: {target}")
    print(f"Python files: {file_count}")
    print(f"Runs per tool: {RUNS} (plus {WARMUP} warmup)")
    print("Clearing caches between runs for fair comparison\n")

    # Clear caches before starting
    clear_caches(project_root)

    # Define cache clearing function
    def clear_cache() -> None:
        clear_caches(project_root)

    tools = [
        ToolInfo("ruff", ["uv", "run", "ruff", "check"], "Linter (no type checking)"),
        ToolInfo("ty", ["uv", "run", "ty", "check"], "Type checker", needs_cache_clear=True),
        ToolInfo("pyrefly", ["uv", "run", "pyrefly", "check"], "Type checker", needs_cache_clear=True),
        ToolInfo("mypy", ["uv", "run", "mypy", "--config-file", "/dev/null",
                         "--ignore-missing-imports", "--no-incremental"],
                 "Type checker (no plugin)", needs_cache_clear=True),
        ToolInfo("mypy + typedframes", ["uv", "run", "mypy", "--no-incremental"],
                 "Type checker + column checker", needs_cache_clear=True),
        ToolInfo("pyright", ["npx", "pyright"], "Type checker", needs_cache_clear=True),
    ]

    # Add typedframes binary if available
    if binary_path.exists():
        tools.insert(0, ToolInfo("typedframes", [str(binary_path)], "DataFrame column checker"))

    results: list[BenchmarkResult] = []

    for tool in tools:
        print(f"Running {tool.name}...", end=" ", flush=True)

        # Use example file for binary, src/ for others
        if "typedframes_linter" in str(tool.cmd[0]):
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
            print(f"{format_time(result.mean)} (±{format_time(result.std)}) [{result.version}]")
        else:
            print(f"SKIPPED: {result.error}")

    # Print summary table
    print("\n" + "=" * 80)
    print("BENCHMARK RESULTS")
    print("=" * 80)
    print(f"{'Tool':<15} {'Version':<15} {'What it does':<25} {'Mean':>10} {'Std':>10}")
    print("-" * 80)

    for result in results:
        if result.success:
            print(
                f"{result.name:<15} {result.version:<15} {result.description:<25} "
                f"{format_time(result.mean):>10} {format_time(result.std):>10}"
            )
        else:
            print(f"{result.name:<15} {'-':<15} {result.description:<25} {'N/A':>10} {'skipped':>10}")

    # Print or update markdown table
    print("\n" + "=" * 80)
    print("MARKDOWN TABLE")
    print("=" * 80)
    print(generate_markdown_table(results, file_count))

    if args.update_readme:
        update_readme(project_root, results, file_count)


if __name__ == "__main__":
    main()

"""Invoke tasks for typedframes development."""

from pathlib import Path

from invoke import Context, Exit, task

RUST_DIR = Path("rust")
BINARY_PATH = RUST_DIR / "target" / "debug" / "typedframes_checker"


def _needs_build() -> bool:
    """Check if rust binary needs rebuilding."""
    if not BINARY_PATH.exists():
        return True

    binary_mtime = BINARY_PATH.stat().st_mtime
    src_dir = RUST_DIR / "src"

    for src_file in src_dir.rglob("*.rs"):
        if src_file.stat().st_mtime > binary_mtime:
            return True

    cargo_toml = RUST_DIR / "Cargo.toml"
    return cargo_toml.exists() and cargo_toml.stat().st_mtime > binary_mtime


@task
def build(ctx: Context, *, force: bool = False) -> None:
    """Build the Rust checker extension (maturin develop) if needed."""
    if force or _needs_build():
        print("Building Rust checker...")
        ctx.run("maturin develop --manifest-path rust/Cargo.toml")
    else:
        print("Rust checker is up to date.")


@task(name="format-check")
def check_if_code_needs_formatting(ctx: Context) -> None:
    """Run ruff format on the codebase."""
    ctx.run("ruff format --check")


@task(name="format")
def format_code(ctx: Context) -> None:
    """Run ruff format on the codebase."""
    ctx.run("ruff format .")


@task
def lint(ctx: Context) -> None:
    """Run all linters: ruff check, ty check, bandit, complexipy, cargo fmt, cargo clippy."""
    ctx.run("ruff check .")
    ctx.run("ty check .")
    ctx.run("bandit -r src/ -c pyproject.toml")
    ctx.run("complexipy src/ --max-complexity-allowed 50")
    print("Checking Rust formatting...")
    ctx.run(f"cd {RUST_DIR} && cargo fmt --all -- --check")
    print("Running Rust clippy...")
    ctx.run(f"cd {RUST_DIR} && cargo clippy -- -D warnings")


@task
def lint_fix(ctx: Context) -> None:
    """Run ruff check with --fix and ruff format."""
    ctx.run("ruff check --fix .")
    ctx.run("ruff format .")


@task(pre=[build])
def test(ctx: Context) -> None:
    """Run pytest with branch coverage and Rust tests. Builds Rust checker if needed."""
    print("Running Python tests...")
    ctx.run("python -m pytest tests/")
    ctx.run("coverage-threshold")
    print("Running Rust tests...")
    ctx.run(f"cd {RUST_DIR} && cargo test")


@task
def docs(ctx: Context) -> None:
    """Serve the MkDocs documentation locally (requires docs dependency group)."""
    ctx.run("mkdocs serve")


@task
def verify_licences(_ctx: Context) -> None:
    """Verify dependency licenses with scancode-toolkit's text-based detection."""
    from scripts.check_licenses import check_licences, format_failure

    print("Checking dependency licenses...")
    failures, checked = check_licences()
    if failures:
        print(f"Disallowed or undetectable licenses in {len(failures)} of {checked} packages:")
        for failure in failures:
            print(format_failure(failure))
        raise Exit(code=1)
    print(f"All {checked} installed packages have an allowed license.")


@task(name="all", pre=[build])
def all_checks(ctx: Context) -> None:
    """Run all checks: format, lint, test, verify-licences."""
    format_code(ctx)
    lint(ctx)
    test(ctx)
    verify_licences(ctx)

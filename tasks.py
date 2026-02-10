"""Invoke tasks for typedframes development."""

from pathlib import Path

from invoke import Context, task

RUST_DIR = Path("typedframes-lint/rust_typedframes_linter")
BINARY_PATH = RUST_DIR / "target" / "debug" / "typedframes_linter"


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
    """Build the Rust linter binary if needed."""
    if force or _needs_build():
        print("Building Rust linter...")
        ctx.run(f"cd {RUST_DIR} && cargo build")
    else:
        print("Rust linter is up to date.")


@task(name="format")
def format_code(ctx: Context) -> None:
    """Run ruff format on the codebase."""
    ctx.run("ruff format .")


@task
def lint(ctx: Context) -> None:
    """Run all linters: ruff check, ty check, bandit, complexipy."""
    ctx.run("ruff check .")
    ctx.run("ty check .")
    ctx.run("bandit -r src/ -c pyproject.toml")
    ctx.run("complexipy src/ --max-complexity-allowed 50")


@task
def lint_fix(ctx: Context) -> None:
    """Run ruff check with --fix and ruff format."""
    ctx.run("ruff check --fix .")
    ctx.run("ruff format .")


@task(pre=[build])
def test(ctx: Context) -> None:
    """Run pytest with branch coverage. Builds Rust linter if needed."""
    ctx.run("python -m pytest tests/")
    ctx.run("coverage-threshold")


@task
def verify_licences(ctx: Context) -> None:
    """Run licensecheck to verify dependency licenses."""
    ctx.run("licensecheck")


@task(name="all", pre=[build])
def all_checks(ctx: Context) -> None:
    """Run all checks: format, lint, test, verify-licences."""
    format_code(ctx)
    lint(ctx)
    test(ctx)
    verify_licences(ctx)

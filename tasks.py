"""Invoke tasks for typedframes development."""

from pathlib import Path

from invoke import Context, task

RUST_DIR = Path("rust_typedframes_linter")
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
        ctx.run("echo 'Building Rust linter...'", pty=True)
        ctx.run("cargo build", cwd=str(RUST_DIR), pty=True)
    else:
        ctx.run("echo 'Rust linter is up to date.'", pty=True)


@task(name="format")
def format_code(ctx: Context) -> None:
    """Run ruff format on the codebase."""
    ctx.run("uv run ruff format .", pty=True)


@task
def lint(ctx: Context) -> None:
    """Run all linters: ruff check, ty check, bandit, complexipy."""
    ctx.run("uv run ruff check .", pty=True)
    ctx.run("uv run ty check .", pty=True)
    ctx.run("uv run bandit -r src/ -c pyproject.toml", pty=True)
    ctx.run("uv run complexipy src/ --max-complexity-allowed 50", pty=True)


@task
def lint_fix(ctx: Context) -> None:
    """Run ruff check with --fix and ruff format."""
    ctx.run("uv run ruff check --fix .", pty=True)
    ctx.run("uv run ruff format .", pty=True)


@task(pre=[build])
def test(ctx: Context) -> None:
    """Run pytest with branch coverage. Builds Rust linter if needed."""
    ctx.run("uv run python -m pytest tests/", pty=True)


@task
def verify_licences(ctx: Context) -> None:
    """Run licensecheck to verify dependency licenses."""
    ctx.run("uv run licensecheck", pty=True)


@task(name="all", pre=[build])
def all_checks(ctx: Context) -> None:
    """Run all checks: format, lint, test, verify-licences."""
    format_code(ctx)
    lint(ctx)
    test(ctx)
    verify_licences(ctx)

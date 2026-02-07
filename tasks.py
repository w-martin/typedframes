"""Invoke tasks for typedframes development."""

from invoke import Context, task


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


@task
def test(ctx: Context) -> None:
    """Run pytest with branch coverage, fail if < 100%."""
    ctx.run("uv run python -m pytest tests/", pty=True)


@task
def verify_licences(ctx: Context) -> None:
    """Run licensecheck to verify dependency licenses."""
    ctx.run("uv run licensecheck", pty=True)


@task(name="all")
def all_checks(ctx: Context) -> None:
    """Run all checks: format, lint, test, verify-licences."""
    format_code(ctx)
    lint(ctx)
    test(ctx)
    verify_licences(ctx)

"""Invoke tasks for typedframes development."""

import importlib.machinery
from pathlib import Path

from invoke import Context, task

RUST_DIR = Path("rust")
EXTENSION_DIR = Path("src") / "typedframes"


def _compiled_extension() -> Path | None:
    """Find the compiled Python extension module actually loaded by `import typedframes`."""
    for suffix in importlib.machinery.EXTENSION_SUFFIXES:
        matches = list(EXTENSION_DIR.glob(f"_rust_checker{suffix}"))
        if matches:
            return matches[0]
    return None


def _needs_build() -> bool:
    """Check if the compiled Python extension needs rebuilding.

    Must check the extension `maturin develop` installs into `src/typedframes/`, not
    `rust/target/debug/typedframes_checker` (the standalone CLI binary `cargo build`/`cargo
    test` produces) -- the two are built independently, so a bare `cargo test` can leave this
    check thinking the extension is fresh when it's actually stale.
    """
    extension = _compiled_extension()
    if extension is None:
        return True

    extension_mtime = extension.stat().st_mtime
    src_dir = RUST_DIR / "src"

    for src_file in src_dir.rglob("*.rs"):
        if src_file.stat().st_mtime > extension_mtime:
            return True

    cargo_toml = RUST_DIR / "Cargo.toml"
    return cargo_toml.exists() and cargo_toml.stat().st_mtime > extension_mtime


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
def verify_licences(ctx: Context) -> None:
    """Verify dependency licenses against the policy in pyproject.toml."""
    ctx.run("trustedlicenses --quiet")


@task(name="all", pre=[build])
def all_checks(ctx: Context) -> None:
    """Run all checks: format, lint, test, verify-licences."""
    format_code(ctx)
    lint(ctx)
    test(ctx)
    verify_licences(ctx)

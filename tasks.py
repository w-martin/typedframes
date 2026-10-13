"""Invoke tasks for typedframes development."""

import importlib.machinery
import sys
from importlib.metadata import version
from inspect import Parameter, isroutine, signature
from pathlib import Path
from types import ModuleType
from typing import Any
from warnings import catch_warnings, simplefilter

from invoke import Context, task

RUST_DIR = Path("rust")
EXTENSION_DIR = Path("src") / "typedframes"
LIBRARY_NAMES = RUST_DIR / "src" / "library_names.txt"


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

    for src_file in [*src_dir.rglob("*.rs"), *src_dir.rglob("*.txt")]:
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


def _targets() -> list[Any]:
    """Modules, classes and live instances whose public names a snippet may use."""
    import numpy as np
    import pandas as pd
    import polars as pl

    frame = pd.DataFrame({"a": [1]})
    series = pd.Series(["a"])
    pl_frame = pl.DataFrame({"a": [1]})
    pl_series = pl.Series("a", ["x"])
    expr = pl.col("a")
    return [
        np,
        np.random,
        np.linalg,
        np.fft,
        np.char,
        np.ma,
        pd,
        pd.api.types,
        pl,
        pl.selectors,
        np.ndarray,
        np.dtype,
        frame,
        series,
        pd.Index,
        pd.MultiIndex,
        pd.DatetimeIndex,
        pd.Timestamp,
        pd.Timedelta,
        pd.Period,
        pd.Categorical,
        frame.groupby("a"),
        frame["a"].rolling(1),
        frame["a"].expanding(),
        frame["a"].ewm(com=1),
        series.str,
        pd.Series(pd.to_datetime(["2020-01-01"])).dt,
        series.astype("category").cat,
        pl_frame,
        pl_frame.lazy(),
        pl_series,
        expr,
        pl_frame.group_by("a"),
        expr.str,
        expr.dt,
        expr.list,
        expr.arr,
        expr.struct,
        expr.cat,
        expr.name,
        expr.bin,
        expr.meta,
        pl_series.str,
        pl_series.dt,
        pl_series.list,
        pl_series.struct,
    ]


def _polars_error() -> type[Exception]:
    """Polars' base exception, imported on demand."""
    from polars.exceptions import PolarsError

    return PolarsError


def _member(target: Any, name: str) -> Any:
    """A target's attribute, or None when reading it needs state the target lacks."""
    with catch_warnings():
        simplefilter("ignore")
        try:
            return getattr(target, name)
        except (AttributeError, ImportError, ValueError, TypeError, NotImplementedError, _polars_error()):
            return None


def _parameter_names(member: Any) -> set[str]:
    """The named parameters of a routine or class, without `self` placeholders and private ones."""
    if not (isroutine(member) or isinstance(member, type)):
        return set()
    try:
        parameters = signature(member).parameters.values()
    except (ValueError, TypeError):
        return set()
    return {
        p.name
        for p in parameters
        if p.kind not in (Parameter.VAR_POSITIONAL, Parameter.VAR_KEYWORD) and not p.name.startswith("_")
    }


_LIBRARY_ROOTS = frozenset({"numpy", "pandas", "polars"})
_MIN_NAME_LENGTH = 2


def _from_library(member: Any) -> bool:
    """Whether a module's attribute is numpy's, pandas' or polars' own, not something it imported."""
    if isinstance(member, ModuleType):
        return member.__name__.split(".")[0] in _LIBRARY_ROOTS
    owner = getattr(member, "__module__", None)
    return not isinstance(owner, str) or owner.split(".")[0] in _LIBRARY_ROOTS


def _library_names() -> tuple[set[str], set[str], set[str]]:
    """The module-level names, the class and instance member names, and the parameter names."""
    module_names: set[str] = set()
    members: set[str] = set()
    keywords: set[str] = set()
    for target in _targets():
        names = module_names if isinstance(target, ModuleType) else members
        for name in dir(target):
            if name.startswith("_") or len(name) < _MIN_NAME_LENGTH:
                continue
            member = _member(target, name)
            if not isinstance(target, ModuleType) or _from_library(member):
                names.add(name)
            keywords |= {k for k in _parameter_names(member) if len(k) >= _MIN_NAME_LENGTH}
    return module_names, members, keywords


@task
def names(_ctx: Context) -> None:
    """Regenerate the library names a bug-report snippet may keep, from the installed numpy, pandas and polars.

    Each line of `rust/src/library_names.txt` is `m<name>` (a name numpy, pandas or polars defines
    in a module), `a<name>` (a class or instance member) or `k<name>` (a parameter name). The
    redactor keeps them only after a dot or as call keywords; names the libraries merely import
    (`os`, `json`) and single letters are left out.
    """
    module_names, members, keywords = _library_names()
    header = (
        f"# Generated by `inv names` from numpy {version('numpy')}, "
        f"pandas {version('pandas')}, polars {version('polars')}. Do not edit."
    )
    lines = [
        header,
        *(f"m{n}" for n in sorted(module_names)),
        *(f"a{n}" for n in sorted(members - module_names)),
        *(f"k{n}" for n in sorted(keywords)),
    ]
    LIBRARY_NAMES.write_text("\n".join(lines) + "\n")
    sys.stdout.write(
        f"wrote {LIBRARY_NAMES}: {len(module_names)} module names, {len(members)} members, {len(keywords)} keywords\n"
    )


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
    # python315.py uses syntax newer than complexipy's parser accepts (see typedframes/compat.py).
    ctx.run("complexipy src/ --max-complexity-allowed 50 --exclude '**/python315.py'")
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

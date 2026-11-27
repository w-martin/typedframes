"""Invoke tasks for typedframes development."""

import sys
from importlib.metadata import version
from inspect import Parameter, isroutine, signature
from os import environ
from pathlib import Path
from shutil import rmtree
from sysconfig import get_config_var, get_path
from types import ModuleType
from typing import Any
from warnings import catch_warnings, simplefilter
from zipfile import ZipFile

from invoke import Context, Exit, task

RUST_DIR = Path("rust")
CRATES_DIR = RUST_DIR / "crates"
CLI_MANIFEST = CRATES_DIR / "cli" / "Cargo.toml"
LIBRARY_NAMES = CRATES_DIR / "domain" / "src" / "library_names.txt"
PACKAGE_DIR = Path("src") / "typedframes"
EXAMPLES_DIR = Path("examples")
TARGET_DIR = Path(environ.get("CARGO_TARGET_DIR", RUST_DIR / "target"))
EXTENSION_BUILD_DIR = TARGET_DIR / "extension"
EXTENSION_SUFFIXES = {".so", ".pyd"}
MAX_RUST_FILE_LINES = 500


def _installed_binary() -> Path | None:
    """The `typedframes` executable `maturin develop` installed into this environment, if any."""
    candidate = Path(get_path("scripts")) / f"typedframes{get_config_var('EXE') or ''}"
    return candidate if candidate.is_file() else None


def _staged_extensions() -> list[Path]:
    """The compiled `typedframes._rust` extension module staged in the package directory, if any (not its stub)."""
    return [path for path in PACKAGE_DIR.glob("_rust.*") if path.suffix in EXTENSION_SUFFIXES]


def _stage_extension(ctx: Context, *, release: bool) -> None:
    """Build the extension module and copy it into the package directory, where the wheel picks it up.

    maturin builds it, not cargo, so the cross-compiling toolchains and the macOS link flags are the ones
    the wheels use. The package's own wheel is built with `bindings = "bin"`, which cannot build it.
    """
    rmtree(EXTENSION_BUILD_DIR, ignore_errors=True)
    profile = "--release" if release else ""
    # From the crate's own directory: maturin would otherwise apply the `bindings = "bin"` of the root pyproject.toml.
    with ctx.cd(RUST_DIR / "python"):
        ctx.run(f'maturin build {profile} -o "{EXTENSION_BUILD_DIR.resolve()}"')
    for stale in _staged_extensions():
        stale.unlink()
    with ZipFile(next(EXTENSION_BUILD_DIR.glob("*.whl"))) as wheel:
        for member in wheel.namelist():
            if Path(member).name.startswith("_rust.") and Path(member).suffix in EXTENSION_SUFFIXES:
                (PACKAGE_DIR / Path(member).name).write_bytes(wheel.read(member))


def _needs_build() -> bool:
    """Check if the installed `typedframes` executable needs rebuilding.

    Must check the executable `maturin develop` installs into the environment, not the one
    `cargo build`/`cargo test` leave in `rust/target/debug`: the two are built independently, so
    a bare `cargo test` can leave this check thinking the installed one is fresh when it is stale.
    """
    binary = _installed_binary()
    extensions = _staged_extensions()
    if binary is None or not extensions:
        return True

    built = min(binary.stat().st_mtime, *(extension.stat().st_mtime for extension in extensions))
    crates = [*sorted(CRATES_DIR.iterdir()), RUST_DIR / "python"]
    sources = [
        *(path for crate in crates for path in (crate / "src").rglob("*") if path.suffix in {".rs", ".txt"}),
        *(crate / name for crate in crates for name in ("build.rs", "Cargo.toml")),
        RUST_DIR / "Cargo.toml",
        RUST_DIR / "Cargo.lock",
    ]
    return any(source.exists() and source.stat().st_mtime > built for source in sources)


@task
def build(ctx: Context, *, force: bool = False) -> None:
    """Build the `typedframes` executable and extension module, and install them with the package, if needed."""
    if force or _needs_build():
        print("Building Rust checker...")
        _stage_extension(ctx, release=False)
        ctx.run(f"maturin develop --manifest-path {CLI_MANIFEST}")
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

    Each line of `rust/crates/domain/src/library_names.txt` is `m<name>` (a name numpy, pandas or polars defines
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


def _long_rust_files() -> list[tuple[Path, int]]:
    """Rust sources, tests included, longer than `MAX_RUST_FILE_LINES`."""
    sources = [*(CRATES_DIR.rglob("*.rs")), *((RUST_DIR / "python").rglob("*.rs"))]
    counts = [(path, len(path.read_text(encoding="utf-8").splitlines())) for path in sorted(sources)]
    return [(path, count) for path, count in counts if count > MAX_RUST_FILE_LINES]


@task
def lint(ctx: Context) -> None:
    """Run all linters: ruff check, ty check, bandit, complexipy, cargo fmt, cargo clippy, Rust file length."""
    ctx.run("ruff check .")
    ctx.run("ty check .")
    ctx.run("bandit -r src/ -c pyproject.toml")
    ctx.run("complexipy src/ --max-complexity-allowed 50")
    print("Checking Rust formatting...")
    ctx.run(f"cd {RUST_DIR} && cargo fmt --all -- --check")
    print("Running Rust clippy...")
    ctx.run(f"cd {RUST_DIR} && cargo clippy --workspace -- -D warnings")
    print(f"Checking Rust files are at most {MAX_RUST_FILE_LINES} lines...")
    if long_files := _long_rust_files():
        listing = "\n".join(f"  {path}: {count} lines" for path, count in long_files)
        message = f"Rust files over {MAX_RUST_FILE_LINES} lines:\n{listing}"
        raise Exit(message, code=1)


@task
def lint_fix(ctx: Context) -> None:
    """Run ruff check with --fix and ruff format."""
    ctx.run("ruff check --fix .")
    ctx.run("ruff format .")


@task(pre=[build])
def test(ctx: Context) -> None:
    """Run pytest with branch coverage and Rust tests. Builds the executable if needed."""
    print("Running Python tests...")
    ctx.run("python -m pytest tests/")
    ctx.run("coverage-threshold")
    print("Running Rust tests...")
    ctx.run(f"cd {RUST_DIR} && cargo test --workspace")


def _tree_size(path: Path) -> int:
    """Disk usage in bytes (allocated blocks, as `du` reports, not apparent size)."""
    files = [path] if not path.is_dir() else [child for child in path.rglob("*") if not child.is_dir()]
    return sum(file.lstat().st_blocks * 512 for file in files)


def _stale_paths() -> list[Path]:
    """Rebuildable output to remove: old Rust build artifacts, a stray `rust/target`, and example virtualenvs."""
    keep = {"debug", "CACHEDIR.TAG", ".rustc_info.json"}
    stale = [path for path in sorted(TARGET_DIR.glob("*")) if path.name not in keep]
    stale += [path for path in [TARGET_DIR / "debug" / "incremental"] if path.exists()]
    local_target = RUST_DIR / "target"
    if local_target.is_dir() and local_target.resolve() != TARGET_DIR.resolve():
        stale.append(local_target)
    stale += sorted(EXAMPLES_DIR.glob("**/.venv"))
    return stale


@task
def clean(_ctx: Context, *, dry_run: bool = False) -> None:
    """Remove rebuildable build output: stale Rust artifacts and the examples' virtualenvs.

    Keeps only the current debug build: deletes incremental compilation caches and every other target
    profile or triple (release, cross targets, extension and wheel staging). Also deletes a leftover
    `rust/target` when `CARGO_TARGET_DIR` points elsewhere, and every `examples/**/.venv` (`uv sync` in
    an example recreates it). Honours `CARGO_TARGET_DIR`, so with a shared target directory it cleans
    that one for every checkout.
    """
    total = 0
    for path in _stale_paths():
        size = _tree_size(path)
        total += size
        sys.stdout.write(f"{'would remove' if dry_run else 'removing'} {path} ({size / 1e6:.0f} MB)\n")
        if dry_run:
            continue
        if path.is_dir():
            rmtree(path)
        else:
            path.unlink()
    sys.stdout.write(f"{'would free' if dry_run else 'freed'} {total / 1e9:.2f} GB\n")


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

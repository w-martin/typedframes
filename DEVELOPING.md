# Developing typedframes

## Prerequisites

- Python 3.11 or higher
- [uv](https://github.com/astral-sh/uv) - Python package manager
- Rust toolchain (for building the linter extension)
- `libmagic` (for the license check only — `brew install libmagic` on macOS,
  `apt-get install libmagic1` on Debian/Ubuntu; see [Verify Licenses](#verify-licenses))

> **macOS note:** If you installed Rust via `brew install rustup` and `rustc`/`cargo`
> are not found even after `rustup toolchain install stable`, it's because Homebrew's
> `rustup` formula is keg-only (it conflicts with the `rust` formula) and only symlinks
> the `rustup` binary itself into your PATH. The actual `rustc`/`cargo`/etc. shims live
> in `$(brew --prefix rustup)/bin` but are never added to PATH automatically, unlike the
> official rustup.rs installer. Fix by adding that directory to your shell profile:
>
> ```shell
> echo 'export PATH="'"$(brew --prefix rustup)"'/bin:$PATH"' >> ~/.zshrc
> source ~/.zshrc
> ```

## Setup

1. Clone the repository:

```shell
git clone https://github.com/w-martin/typedframes.git
cd typedframes
```

2. Install dependencies and build the Rust extension:

```shell
uv sync
uv run maturin develop
```

3. Install pre-commit hooks:

```shell
uv run pre-commit install
```

## Task Commands

All development tasks are managed via [Invoke](https://www.pyinvoke.org/). Run `uv run inv --list` to see available tasks.

### Format Code

```shell
uv run inv format
```

Runs `ruff format` on the entire codebase.

### Lint Code

```shell
uv run inv lint
```

Runs all linters:
- `ruff check` - Python linting
- `ty check` - Type checking
- `bandit` - Security linting
- `complexipy` - Complexity checking on `src/` (fails a function above 50); a separate,
  stricter mccabe check runs inside `ruff check` (fails above 10) and covers both `src/`
  and `rust/`'s Python-facing glue

### Fix Linting Issues

```shell
uv run inv lint-fix
```

Runs `ruff check --fix` and `ruff format` to auto-fix issues.

### Run Tests

```shell
uv run inv test
```

Runs pytest with branch coverage, then the Rust test suite. Coverage must be 100% or the
build fails.

The Rust tests can also be run on their own:

```shell
cd rust && cargo test
```

This needs the project virtualenv to exist, because pyo3 links against a real
interpreter and the `abi3-py311` floor requires Python 3.11 or newer. Run `uv sync`
first (see [Setup](#setup)) and `cargo test` picks it up automatically, via
`PYO3_PYTHON` in `rust/.cargo/config.toml`.

Without it you'll see one of:

```
error: failed to run the Python interpreter at .../rust/../.venv/bin/python: No such file or directory
error: cannot set a minimum Python version 3.11 higher than the interpreter version 3.9
```

The first means the venv isn't there yet — run `uv sync`. The second means something
else already set `PYO3_PYTHON`, or an older interpreter is being used; note that macOS
ships Python 3.9 as its system `python3`.

If your interpreter lives somewhere other than `.venv/`, set `PYO3_PYTHON` yourself and
it takes precedence over the checked-in default:

```shell
PYO3_PYTHON=/path/to/python3.11 cargo test
```

### Verify Licenses

```shell
uv run inv verify-licences
```

Checks the license of every package installed in the development environment using
[scancode-toolkit](https://github.com/aboutcode-org/scancode-toolkit). Unlike a
metadata-only checker, scancode matches the actual license *text* a package ships
against a database of ~2500 known licenses — the same approach GitHub's own license
detector takes — so a package whose declared metadata is missing, stale, or vague is
still identified from the LICENSE file it bundles.

The policy lives in `scripts/check_licenses.py`. A package passes when at least one
detected license falls into an allowed category: `Permissive`, `Public Domain` or
`Copyleft Limited` (LGPL, MPL, EPL). Strong copyleft (GPL, AGPL), proprietary and
undetectable licenses fail the build, which matches what this MIT-licensed project
could actually depend on.

The check requires at least one allowed license rather than the absence of a
disallowed one, because packages routinely concatenate the license texts of their
vendored dependencies into their own LICENSE file — pandas ships BSD-3-Clause
alongside notices that include GPL text, and the grant pandas makes to us is
BSD-3-Clause. A package offering only a disallowed license, or none at all, still
fails. `IGNORED_PACKAGES` in that module is the escape hatch for a package whose
license genuinely cannot be determined from the installed artifact.

> **macOS/ARM note:** scancode depends on `libmagic`, and its bundled
> `typecode-libmagic` plugin has no real arm64 build (the `macosx_11_0_arm64` wheel is
> a 1 KB placeholder). On Apple Silicon, install libmagic yourself or the check fails
> at import with `NoMagicLibError`:
>
> ```shell
> brew install libmagic
> ```
>
> On Debian/Ubuntu the equivalent is `apt-get install libmagic1`. CI installs this
> automatically (see `.github/workflows/publish.yml`) before running the check.

### Run All Checks

```shell
uv run inv all
```

Runs all checks in order: format, lint, test, verify-licences.

## Pre-commit Hooks

Pre-commit hooks run automatically on `git commit`:

- Trailing whitespace removal
- End-of-file fixer
- YAML syntax check
- Large file check
- Ruff linting with auto-fix
- Ruff formatting

To run hooks manually on all files:

```shell
uv run pre-commit run --all-files
```

## Coverage Requirements

This project requires 100% branch coverage. The coverage configuration excludes:
- `TYPE_CHECKING` blocks
- `raise NotImplementedError` statements
- Lines marked with `# pragma: no cover`

## Building the Rust Linter

The Rust linter extension is built with maturin:

```shell
uv run maturin develop
```

For release builds:

```shell
uv run maturin build --release
```

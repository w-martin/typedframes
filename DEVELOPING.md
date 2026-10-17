# Developing typedframes

## Prerequisites

- Python 3.11 or higher
- [uv](https://github.com/astral-sh/uv) - Python package manager
- Rust toolchain (for building the `typedframes` executable)

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

2. Install dependencies. This builds the Rust `typedframes` executable and installs it into the
   project environment next to the Python package (the mypy plugin and `python -m typedframes` run it):

```shell
uv sync
```

   After editing Rust code, rebuild and reinstall it with `uv run inv build`.

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

The Rust tests need no Python. They include end-to-end tests of the `typedframes` command
(`rust/tests/cli.rs`).

### Verify Licenses

```shell
uv run inv verify-licences
```

Checks the license of every package installed in the development environment using
[trustedlicenses](https://github.com/w-martin/trustedlicenses). It checks a
package's declared metadata first, falling back to text-matching its bundled
license file when metadata is missing, stale, or too vague to act on.

The policy lives in `[tool.trustedlicenses]` in `pyproject.toml`. A package passes
when at least one detected license falls into an allowed category: `Permissive`,
`Public Domain` or `Copyleft Limited` (LGPL, MPL, EPL). Strong copyleft (GPL, AGPL),
proprietary and undetectable licenses fail the build, which matches what this
MIT-licensed project could actually depend on. `ignored-packages` in the same table
is the escape hatch for a package whose license genuinely cannot be determined from
the installed artifact.

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

## Building the `typedframes` executable

The command line is a Rust program, `typedframes`. It is shipped in the wheel next to the Python
package (the schema classes and the mypy plugin) and built with maturin:

```shell
uv run maturin develop
```

builds it in debug mode and installs it into the environment (`uv run inv build` does the same
when the Rust sources changed). For a release build of the wheel:

```shell
uv run maturin build --profile cli
```

The `cli` profile (`rust/Cargo.toml`) is the release profile with `panic = "abort"` and `opt-level = "s"`:
a much smaller executable, at some cost in speed. The extension module is built with `--release` and unwinds,
because a panic there must become a Python exception.

To run the executable without installing it, `cargo run --manifest-path rust/Cargo.toml -- check .`.

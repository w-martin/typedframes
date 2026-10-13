# v0.13.0 release notes (do not commit this file)

## Commit message

```
v0.13.0 Add report --all and returned-frame narrowing, and make check faster

report:
- `report --all PATH` groups every untyped site by why the checker decided as it did
  (`--outcome`, `--format json`, `--output-dir`).
- Snippets are cut down to the statements the verdict depends on, so large scopes no
  longer get refused. Library names come from a generated list (`inv names`) and are kept
  only after a dot or as call keywords.

checker:
- A `columns=` argument that is present but not literal no longer gets "specify columns=".
- `self.method()` resolves its library from the method's return annotation, `self.method(df)`
  reads are followed, and a single direct `return pd.DataFrame({...})` or
  `return pd.read_csv(..., usecols=[...])` gives callers its columns.
- Class constants read through `self`/`cls` are folded, unless a subclass in the file
  rebinds them, the class assigns attributes dynamically, or the value is an empty
  placeholder. Without `Final`, an `assumed-constant` info diagnostic says so.
- `suggest` narrows a returned frame to the columns every caller in the scanned project
  reads, as a `review` suggestion. Single-file indexes and `--no-index` never do this.

performance (Great Expectations, 494 files: 121 ms -> 56 ms, peak memory 38 -> 43 MB):
- Indexing parses and analyses each file once instead of three times for a plain `check`.
  The called-functions pass uses call sites recorded during indexing instead of re-reading
  every file, and runs in parallel.
- The check phase parses once and resolves the project root and settings once per directory.
- Files are scheduled largest first, and the index just built is reused for the check
  instead of being serialised and decoded again.
- `suggest` and `report` check files in parallel (`suggest` on the same project: about
  290 -> 135 ms).
- Constant folding rewrites the tree in place, the evaluator borrows names instead of
  allocating them, and its tables use a fast in-crate hasher.
- The extension uses jemalloc (mimalloc on Windows), tuned by the CLI for a short run.
- The CLI no longer imports dataclasses or the schema classes.
```

## PR title

`v0.13.0: report --all, returned-frame narrowing, faster check`

## PR body

```
Adds `typedframes report --all`, makes bug-report snippets minimal, and lets `suggest` see
through a few more patterns.

**report**
- `report --all PATH` groups all untyped sites by verdict and reason, with the smallest
  example per group. `--outcome`, `--format json` and `--output-dir` are available.
- Snippets are reduced by deleting statements and parameters while the checker verdict is
  unchanged, so a 160-line scope is no longer refused and a 1400-line one takes 0.2s.
- Library names are generated from the installed numpy/pandas/polars (`uv run inv names`)
  instead of a hand-written list. They survive redaction only after a dot or as call
  keywords, or when a kept import brought them in; user definitions are always renamed.

**checker**
- Hint for a non-literal `columns=` no longer asks for `columns=`.
- Method calls: `self.method()` library from the return annotation; `self.method(df)`
  reads followed (not when the name is defined by two classes, or the method is
  decorated); a single direct frame return gives callers its columns (also across files
  through `class_methods`).
- `self.NAME` class constants are folded and an `assumed-constant` info diagnostic
  suggests `Final`. Skipped when a same-file subclass rebinds the name, the class uses
  `setattr`/`__dict__`/`__setattr__`, or the value is an empty collection.
- Returned frames: every call to a project function is observed; when each is an
  `x = name(...)` whose result is only read by named columns, `suggest` narrows the
  returned frame to those columns (review tier, with a note). Only with a whole-project
  index; aliased imports, module-level results, loops, decorated functions and functions
  passed as values keep the function withheld.

**performance**
`check` takes about half the time of 0.12.0 for slightly more memory. For a plain `check`,
indexing parses and analyses each file once instead of three times (`suggest`/`report` still
read files once more to observe callers, and files with Feast-governed functions are re-read
for those). The check phase parses once and resolves the project root and `pyproject.toml`
once per directory; the pass that finds called functions (about a third of the old run, and
run even on a fully cached project) now uses call sites recorded while indexing and runs in
parallel; files are scheduled largest first; the built index is reused for the check instead
of being serialised and decoded again; constant folding rewrites the syntax tree in place; the
evaluator borrows names and its tables use a fast in-crate hasher; `suggest` and `report`
check files in parallel; the CLI no longer imports `dataclasses` or the schema classes; and
the extension uses jemalloc (mimalloc on Windows), as ruff does.
Benchmark (median of 5, Great Expectations 494 files, through `uv run`): 72.4 ms, was 122.1 ms.
Directly, `typedframes check` takes 56 ms against 121 ms, at 43 MB peak against 38 MB. Index,
check, `suggest` and `report --all` output are identical to a reference build on Great
Expectations and this repo, over three runs each.

**not covered**
Config objects such as `self.cfg.col` (instance attributes) are still untracked.
`cargo audit` and the TestPyPI dry run were not run locally; CI runs the former.

Release builds: jemalloc needs `JEMALLOC_SYS_WITH_LG_PAGE=16` for the aarch64 Linux wheel (set in
`publish.yml`, as ruff does). It was not built on Linux, Windows or x86_64 macOS locally, so the
first tag build is its first test there. Windows uses mimalloc, which on macOS raised peak memory
about threefold at 18 threads; Windows was not measured. jemalloc is BSD-2-Clause, whose notice
requirement for binary distribution is not yet met by a third-party notice file.

Local checks: `uv run inv all`; `cargo fmt --check`; `cargo clippy -- -D warnings`;
Python 3.11 test run; reviewed by an independent pass over the diff.
```

## Tag message (state it, then wait for approval before tagging)

Proposal: `v0.13.0 Add report --all, minimal bug-report snippets and returned-frame narrowing`

## Release steps left for you

1. Create the branch, stage the changed files, commit with the message above.
2. Open the PR; `gh pr checks --watch` until green (CI runs `cargo audit`, which was not run locally).
3. Merge, then tag `v0.13.0` (the workflow's `verify-version` job checks it against `pyproject.toml`).
4. Afterwards, check that the wheels, sdist and GitHub release appear, and that `test-published` passes.

## Local-only caveat

Release builds with `strip = "symbols"` produce a Mach-O this machine's `dlopen` rejects
("mis-aligned LINKEDIT string pool"), so local release builds and the benchmark were run
with `CARGO_PROFILE_RELEASE_STRIP=none`. Nothing in the repository was changed for that.

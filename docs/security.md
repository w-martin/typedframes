# Security and data handling

What `typedframes` reads, runs, writes and connects to, so you can judge whether it is safe to run
on your code and check each claim yourself.

## What it does with your code

- **It reads source, and never runs it.** `check`, `suggest` and `report` parse Python files into
  syntax trees and analyse those. Nothing in your project is imported or executed, and the
  checker's constant evaluation interprets a small subset of expressions itself rather than calling
  Python.
- **Files it reads:** the path you give it and the other `.py` files under the project root (minus
  the excluded directories), `pyproject.toml` for configuration, and, only for packages you list in
  `trace_external_packages`, those packages' source in the project's `.venv`.
- **It makes no network connection.** There is no networking code in the checker or the CLI and no
  telemetry. The Python package has no runtime dependencies (optional extras aside); the Rust
  extension is statically linked from the crates in `rust/Cargo.lock`, which include none of the
  common HTTP or TLS crates, and the ruff parser crates are pinned to a release tag.
- **It starts one kind of subprocess: `git`,** and only for `suggest --apply` and `--interactive`,
  to check that the work tree is clean and the files are tracked.
- **Environment variables read:** `TYPEDFRAMES_JOBS` (worker count) and `TYPEDFRAMES_CACHE_DIR`
  (cache location), plus `XDG_CACHE_HOME` or `LOCALAPPDATA` to find the default cache directory.

## What it writes

- **The index cache.** To skip unchanged files on later runs, `check`, `suggest` and `report` keep a
  per-file index in a cache directory: the per-user cache directory by default, or the one set by
  `--cache-dir`, `TYPEDFRAMES_CACHE_DIR` or `cache_dir` in `[tool.typedframes]`. It holds the
  **absolute paths of your source files and the class, function, column and schema names found in
  them**, in files named `index-*.bin`. Cache files are readable by the user alone (mode 0600), and
  the default directory is made owner-only (0700); a directory you chose is not changed, because it
  may be shared. `--no-cache` skips the cache for a run, `typedframes cache path` shows where it is
  and `typedframes cache clear` deletes the tool's own files from it.
- **Edits to your files, only on request.** `suggest --apply` and `-i` are the only commands that
  change project files. They need a clean git work tree and tracked files (`--allow-dirty` skips
  this), re-check each edited file in memory before writing, and write atomically. `typedframes
  cache gitignore` appends one line to `.gitignore`.
- Everything else prints to standard output and standard error.

## `report` is the one command that emits code-derived text

`typedframes report NAME` prints a bug-report body for pasting into an issue, with a redacted copy of
the code around one site. Nothing runs it automatically: not `check`, the pre-commit hook or the
GitHub Action. The redaction is best-effort and is not anonymisation; what it replaces and what it
keeps is described under [Reporting a problem](usage.md#reporting-a-problem-report). Do not use it
on code you are not allowed to share.

## Checking it yourself

- Run it with the network denied, for example on macOS:
  `sandbox-exec -p '(version 1)(allow default)(deny network*)' typedframes check src/`
  or in any network-isolated container. It works the same.
- Run `typedframes cache path`, look in that directory, and read the file names.
- Run `git status` after a `check` or `suggest` without `--apply`: no tracked file changes.
- Read `rust/Cargo.lock` for the full dependency list.

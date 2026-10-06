# Type stub for the compiled Rust extension (`_rust_checker.abi3.so`).
#
# Mirrors the `#[pyfunction]`s registered in `rust/src/lib.rs`'s `#[pymodule]` block;
# signatures come from their `#[pyo3(signature = ...)]` declarations in
# `rust/src/pyapi.rs`. Each returns a JSON (or, for the index builders, MessagePack)
# payload as raw `str`/`bytes` rather than a typed structure -- the Rust side owns the
# schema, Python only decodes it -- so these signatures cover call boundaries, not
# payload shape.

def check_file(file_path: str, index_bytes: bytes | None = None) -> str: ...
def check_notebook(file_path: str, index_bytes: bytes | None = None) -> str: ...
def check_files(file_paths: list[str], index_bytes: bytes | None = None) -> list[tuple[str | None, str | None]]: ...

# Dry-run suggestions as JSON: `{"suggestions": [...], "withheld": [...]}`. `display_path`
# only names the file in diff headers; nothing is written.
def suggest_file(file_path: str, display_path: str, index_bytes: bytes | None = None) -> str: ...
def apply_suggestions(
    requests: list[tuple[str, str, list[tuple[int, int]] | None]],
    index_bytes: bytes | None = None,
    require_tracked: bool = False,
) -> str: ...
def vcs_problem(root: str) -> str | None: ...
def build_project_index(project_root: str, cache_dir: str | None = None) -> bytes: ...

# `None` when no `pyproject.toml` exists anywhere above `file_path` -- there is then no
# project root to resolve the file's imports against, so there is no index to build.
def build_single_file_index(file_path: str, cache_dir: str | None = None) -> bytes | None: ...

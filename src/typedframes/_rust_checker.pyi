# Type stub for the compiled Rust extension (`_rust_checker.abi3.so`).
#
# Mirrors the four `#[pyfunction]`s registered in `rust/src/lib.rs`'s `#[pymodule]`
# block; signatures come from their `#[pyo3(signature = ...)]` declarations in
# `rust/src/pyapi.rs`. Each returns a JSON (or, for the index builders, MessagePack)
# payload as raw `str`/`bytes` rather than a typed structure -- the Rust side owns the
# schema, Python only decodes it -- so these signatures cover call boundaries, not
# payload shape.

def check_file(file_path: str, index_bytes: bytes | None = None) -> str: ...
def check_notebook(file_path: str, index_bytes: bytes | None = None) -> str: ...
def build_project_index(project_root: str) -> bytes: ...

# `None` when no `pyproject.toml` exists anywhere above `file_path` -- there is then no
# project root to resolve the file's imports against, so there is no index to build.
def build_single_file_index(file_path: str) -> bytes | None: ...

# Examples

## multi_file_inference/

The checker works from day one with no schema classes.  `loaders.py` and
`transforms.py` use `usecols=` / `columns=` / `dtype=` to give the checker
column information; method chains (rename, drop, assign, select) propagate that
information forward.  Two intentional bugs are caught as unknown-column errors.

`pipeline.py` shows the ceiling of inference alone: `load_orders` returns plain
`pd.DataFrame`, so the checker has nothing to validate against at the call site
— a wrong column name passes silently.

```shell
uv run typedframes check multi_file_inference/
```

## multi_file_with_schema/

The same three-file layout, but `schemas.py` defines `BaseSchema` classes and
`loaders.py` annotates its return types as `Annotated[pd.DataFrame, OrderSchema]`.
The checker builds a project index and resolves those annotations at every call
site.

**One error, three files.**  The single bug — `orders["revenue"]` in
`pipeline.py` — requires tracing from `pipeline.py` → `loaders.py` →
`schemas.py` to detect.  The inference example reported two errors across two
files and was blind to this one.

This is the payoff of investing in schemas as a codebase matures: the checker
has complete information, emits no spurious warnings, and catches bugs that
span module boundaries.

```shell
uv run typedframes check multi_file_with_schema/
```

## inference_example.py

Single-file walkthrough of all inference modes: full schema annotation,
`usecols=` inference, bare load (`untracked-dataframe` — a warning by default,
downgrade to an info-level note with `--lenient-ingest`), and method-chain propagation
through rename / drop / assign / filter.

```shell
uv run mypy --config-file mypy_empty.ini --strict inference_example.py
uv run ty check inference_example.py
uv run typedframes check inference_example.py
```

## annotated_pandas_example.py

`Annotated[pd.DataFrame, Schema]` basics: string column access, descriptor
`.s` for refactor-safe names, and intentional errors to show what the checker
catches. Plain `pd.DataFrame` throughout — no `PandasFrame` subclass involved,
despite the similar-sounding older filename this replaces
(`pandasframe_example.py`). `PandasFrame`/`PolarsFrame` are deprecated — see
`docs/api/pandas.md`/`polars.md` for why — so this `Annotated[...]` form is the
only pattern demonstrated going forward.

```shell
uv run mypy --config-file mypy_empty.ini --strict annotated_pandas_example.py
uv run ty check annotated_pandas_example.py
uv run typedframes check annotated_pandas_example.py
```

## annotated_polars_example.py

`Annotated[pl.DataFrame, Schema]` with validated `pl.col()` references, polars
expressions via `Schema.col`, and intentional errors.

```shell
uv run mypy --config-file mypy_empty.ini --strict annotated_polars_example.py
uv run ty check annotated_polars_example.py
uv run typedframes check annotated_polars_example.py
```

## annotated_dask_example.py

**Experimental.** `dask.dataframe` as a third backend: inference from
`dd.read_csv(usecols=...)`, structural operations (drop / assign) on a dask frame,
column tracking surviving the lazy-to-eager `.compute()` step,
`dd.from_pandas(...)` carrying an in-memory frame's columns across, and
`Annotated[dd.DataFrame, Schema]` for the annotated form. Four intentional
unknown-column errors and one `untracked-dataframe` warning.

Run without `--strict`, unlike the pandas/polars examples above: dask ships a
`py.typed` marker but its annotations are incomplete (`dd.read_csv` returns `Any`,
`dd.from_pandas` is untyped), so `--strict` reports `no-any-return` /
`no-untyped-call` against dask's own API rather than anything in this file.

```shell
uv run mypy --config-file mypy_empty.ini annotated_dask_example.py
uv run ty check annotated_dask_example.py
uv run typedframes check annotated_dask_example.py
```

## schema_algebra_example.py

Schema composition via inheritance and the `+` operator.  Shows how to build
merged schemas for joined DataFrames without re-listing columns, using
`Annotated` and `.s` for the merge key.

```shell
uv run mypy --config-file mypy_empty.ini --strict schema_algebra_example.py
uv run ty check schema_algebra_example.py
uv run typedframes check schema_algebra_example.py
```

## pandera_example.py

Converts a `BaseSchema` to a Pandera schema with `to_pandera_schema()` for
runtime validation.  Use alongside the standalone checker: typedframes catches
column errors at lint time, Pandera validates actual data at runtime.

```shell
uv run mypy --config-file mypy_empty.ini --strict pandera_example.py
uv run ty check pandera_example.py
uv run typedframes check pandera_example.py
```

## typedframes_example.py

Quick-start showing pandas and polars side-by-side with a shared schema.
Demonstrates both `Annotated` annotation and descriptor `.s` / `.col` access.

```shell
uv run mypy --config-file mypy_empty.ini --strict typedframes_example.py
uv run ty check typedframes_example.py
uv run typedframes check typedframes_example.py
```

## ipynb_example.ipynb

`typedframes check` reads `.ipynb` files directly — no conversion step. Notebooks are
parsed and checked entirely in Rust via [`ruff_notebook`](https://github.com/astral-sh/ruff/tree/main/crates/ruff_notebook)
— the same crate Ruff and Pyrefly use for their own notebook support — so IPython magics
and shell escapes (`%matplotlib inline`, `!pip install ...`) parse natively rather than
needing to be stripped, and every result (including a same-notebook schema's "defined at
..." cross-reference) is mapped back to the cell it came from: `notebook.ipynb:cell
N:line:col`. `cell` counts every cell in the notebook, markdown included — the same
convention `ruff_notebook`'s own `NotebookIndex` uses — so a code cell's number is its
position in the notebook, not "the Nth code cell".

This notebook defines a schema and a valid `orders` DataFrame, uses an IPython magic
alongside real code to show it's tolerated, then demonstrates every diagnostic severity
the checker produces: an `untracked-dataframe` warning (`pd.read_csv(...)` with no
`usecols=`/schema), a `dropped-unknown-column` warning (`drop(columns=[...])` naming a
column `Orders` doesn't have), and finally an `unknown-column` error
(`orders["revenue"]`, where the real column is `amount`). The three flagged cells are
left unexecuted, since running any of them would raise at runtime — that's the whole
point: the checker catches all three without running anything.

```shell
uv run typedframes check ipynb_example.ipynb
```

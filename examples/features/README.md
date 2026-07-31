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
`usecols=` inference, bare load (`untracked-dataframe` — off by default, enable with
`--strict-ingest`), and method-chain propagation through rename / drop /
assign / filter.

```shell
uv run mypy --config-file mypy_empty.ini --strict inference_example.py
uv run ty check inference_example.py
uv run typedframes check inference_example.py
```

## pandasframe_example.py

`Annotated[pd.DataFrame, Schema]` basics: string column access, descriptor
`.s` for refactor-safe names, and intentional errors to show what the checker
catches.

```shell
uv run mypy --config-file mypy_empty.ini --strict pandasframe_example.py
uv run ty check pandasframe_example.py
uv run typedframes check pandasframe_example.py
```

## polarsframe_example.py

`Annotated[pl.DataFrame, Schema]` with validated `pl.col()` references, polars
expressions via `Schema.col`, and intentional errors.

```shell
uv run mypy --config-file mypy_empty.ini --strict polarsframe_example.py
uv run ty check polarsframe_example.py
uv run typedframes check polarsframe_example.py
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

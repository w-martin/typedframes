# cudf/

Native `cudf.DataFrame` tracking (experimental). cuDF uses pandas' column-access idiom
-- plain string subscripts -- so the checker's pandas rules for `rename`, `drop`,
`assign`, `merge` and `Annotated[cudf.DataFrame, Schema]` apply unchanged. See
`example.py`'s docstring for the reader-surface differences (`read_sql`/`read_excel`
don't exist on cuDF) the checker accounts for.

```shell
uv run typedframes check examples/backends/cudf/example.py
```

## example.py vs. run_example.py

`example.py` is a **linter fixture**: several of its calls (a misspelled column, a
merge against a column in neither schema, `cudf.read_sql`, which cuDF does not export)
are deliberately invalid, to demonstrate what the checker catches. It is checked, never
executed.

`run_example.py` is a **genuinely runnable program**: it generates its own small
synthetic Parquet/CSV fixtures and exercises the same schema-shaped pipeline using only
the valid operations, to demonstrate that the API usage `example.py` is built on is
real cuDF/pandas usage and not something invented to trip the linter.

## Running run_example.py for real

```shell
docker-compose up --build
```

**This has not been run or verified in this repository.** cuDF requires an NVIDIA GPU
and CUDA runtime; the machine this was authored on is Apple Silicon (no NVIDIA GPU
exists, and neither Docker Desktop nor colima on macOS can pass through a GPU that
isn't there). The Dockerfile is built on a real, currently-published RAPIDS image
(`rapidsai/base:26.08-cuda12-py3.11`, confirmed against Docker Hub) and the
docker-compose GPU reservation follows the documented NVIDIA Container Toolkit
pattern, but actually running this container requires a host with a real NVIDIA GPU
and the NVIDIA Container Toolkit installed. If you have that hardware, please verify
it and report back rather than trusting this note alone.

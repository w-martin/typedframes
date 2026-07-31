# Library Comparisons

Each directory compares typedframes against one other dataframe-validation library:
what it catches, what it misses, and where the two are complementary rather than
competing. Every directory is a self-contained `uv` project (`pyproject.toml` +
`uv.lock`) with its own `SPEC.md` write-up and `a_working_example.py` /
`b_static_analysis.py` / `c_typedframes_comparison.py` triad.

| Directory | Compared against |
|---|---|
| [`dataenforce/`](dataenforce/) | [dataenforce](dataenforce/SPEC.md) — `Dataset["col":type]` runtime decorator |
| [`dataframely/`](dataframely/) | [dataframely](dataframely/SPEC.md) — polars-only runtime validation with generics |
| [`great_expectations/`](great_expectations/) | [Great Expectations](great_expectations/SPEC.md) — runtime data-quality framework |
| [`narwhals/`](narwhals/) | [narwhals](narwhals/SPEC.md) — cross-backend dataframe API (orthogonal, not competing) |
| [`pandas_stubs_example/`](pandas_stubs_example/) | [pandas-stubs](pandas_stubs_example/SPEC.md) — mypy/pandas-stubs alone, no column-level checking |
| [`pandas_type_checks/`](pandas_type_checks/) | [pandas-type-checks](pandas_type_checks/SPEC.md) |
| [`pandera/`](pandera/) | [Pandera](pandera/SPEC.md) — runtime schema validation |
| [`patito/`](patito/) | [patito](patito/SPEC.md) — Pydantic-based, polars-first runtime validation |
| [`static_frame/`](static_frame/) | [StaticFrame](static_frame/SPEC.md) — type safety via replacing pandas entirely |
| [`strictly_typed_pandas/`](strictly_typed_pandas/) | [strictly-typed-pandas](strictly_typed_pandas/SPEC.md) |

Run any directory's checks with:

```shell
cd examples/comparisons/<name>
uv run mypy --config-file mypy.ini --strict a_working_example.py
uv run mypy --config-file mypy_typedframes.ini --strict c_typedframes_comparison.py
uv run typedframes check .
```

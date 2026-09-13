# Backends

Column tracking through a DataFrame library's own native API -- the library's real
method names and its own idiom for referring to a column, rather than a SQL `SELECT`
list (`sql_connectors/`) or pandas/polars `usecols=`/`columns=` (`features/`).

Nothing in this group is executed by `typedframes check` itself, and none of these
libraries need to be installed for the checker to work on code that uses them: the
checker reads the source. Each directory additionally ships a `run_example.py` and a
`docker-compose.yml` for actually running the real library against synthetic fixture
data, separate from the checker.

| Directory | Backend | Status |
|---|---|---|
| [`cudf/`](cudf/) | `cudf.DataFrame` | Experimental |

## cudf/

Native `cudf.DataFrame` tracking: pandas' column-access idiom and structural-operation
signatures (`rename`, `drop`, `assign`, `merge`) apply unchanged, since cuDF mirrors
pandas there. `Annotated[cudf.DataFrame, Schema]` is tracked the same way as pandas.

```shell
uv run typedframes check examples/backends/cudf/
```

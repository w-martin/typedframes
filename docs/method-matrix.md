# AST Method Matrix

This page documents how the `typedframes` static checker handles each DataFrame operation.
Operations fall into three categories: **schema-modifying** (the checker updates its
internal column model), **row-passthrough** (the checker assumes the schema is unchanged),
and **untracked** (the variable is dropped from tracking to avoid false positives).

The tables below list every spelling the checker recognizes across the supported
backends, so some rows are pandas-only (`query`, `nlargest`) and some are polars-only
(`sort`, `select(pl.col(...))`, `with_columns`). A row applies to your code only if your
backend actually provides that method.

RAPIDS cuDF (experimental) uses pandas' string-subscript column access and pandas'
signatures for `rename`, `drop`, `assign`, `pop`, `insert` and `merge`, so the
pandas rows apply to it unchanged. cuDF implements no `filter`, `select`, `sort` or
`with_columns`, and its reader surface is a subset of pandas': the checker recognizes
`cudf.read_csv`, `read_parquet`, `read_json`, `read_orc`, `read_avro`, `read_feather`
and `read_hdf`, and deliberately does not recognize `read_sql`, `read_excel` or
polars' `scan_*` functions on a `cudf` receiver, because cuDF exports none of them.
cuDF is eager on a single GPU — there is no `collect()`/`compute()` materialization step
to track, unlike polars' `LazyFrame`.

---

## Schema-Modifying Operations

The checker updates its column model when it sees these operations, so subsequent accesses
are validated against the new schema.

| Operation | Effect on schema | Example |
|-----------|-----------------|---------|
| `df["col"] = val` | Adds `"col"` to the schema | `df["score"] = df["value"] * 2` |
| `del df["col"]` | Removes `"col"` from the schema | `del df["temp"]` |
| `df.drop(columns=[…])` | Removes listed columns | `df.drop(columns=["a", "b"])` |
| `df.drop([…])` | Removes listed columns (positional) | `df.drop(["a", "b"])` |
| `df.assign(col=…)` | Adds new column(s) to the schema | `df.assign(full_name=…)` |
| `df.rename(columns={…})` | Renames columns in the schema | `df.rename(columns={"a": "b"})` |
| `df.select([…])` | Narrows schema to selected columns | `df.select(["id", "name"])` |
| `df.select(pl.col("…"))` | Narrows schema to the named column | `df.select(pl.col("id"))` |
| `df.pop("col")` | Removes `"col"` from the schema | `df.pop("score")` |
| `df.insert(pos, "col", val)` | Adds `"col"` to the schema | `df.insert(0, "rank", …)` |
| `df[["c1", "c2"]]` | Narrows schema to selected columns | `subset = df[["id", "name"]]` |
| `pd.merge(left, right, …)` | Merges both schemas | `merged = pd.merge(a, b, on="id")` |
| `pd.concat([df1, df2], …)` | Unions both schemas | `combined = pd.concat([a, b])` |

---

## Row-Passthrough Operations

The checker leaves the schema unchanged for these operations — the output variable inherits
the same column model as the input.

| Operation | Notes |
|-----------|-------|
| `df.filter(…)` | Row filter; columns unchanged |
| `df.query(…)` | pandas query string; columns unchanged |
| `df.head(n)` | First *n* rows; columns unchanged |
| `df.tail(n)` | Last *n* rows; columns unchanged |
| `df.sample(…)` | Random sample; columns unchanged |
| `df.sort_values(…)` | Row sort; columns unchanged |
| `df.sort(…)` | polars row sort; columns unchanged |
| `df.reset_index(…)` | Index reset; columns unchanged |
| `df.nlargest(n, col)` | Top *n* rows; columns unchanged |
| `df.nsmallest(n, col)` | Bottom *n* rows; columns unchanged |
| `df.fillna(…)` | Fill NaN values; columns unchanged |
| `df.dropna(…)` | Drop NaN rows; columns unchanged |
| `df.ffill()` / `df.bfill()` | Forward/back fill; columns unchanged |

---

## Untracked Operations

For these operations the result variable is **not tracked** — the checker won't report
false positives on it, but it also won't validate column references against it.

These operations require runtime information (joined keys, pivot categories, melt id-vars,
explosion depth, etc.) that is not available to a static AST pass. Tracking them correctly
would require evaluating expressions at compile time, which is out of scope for a static
checker.

| Operation | Why untracked |
|-----------|--------------|
| `df.join(other, …)` | Output schema depends on join keys and `how=` parameter |
| `df.merge(other, …)` | (pandas instance method) Same as join |
| `df.pivot(…)` | Output columns are derived from cell values at runtime |
| `df.pivot_table(…)` | Same as pivot |
| `df.melt(…)` | Converts columns to rows; output schema varies by `id_vars` |
| `df.explode(col)` | Schema depends on list column depth |
| `pd.get_dummies(df, …)` | Columns come from categorical values, unknown at lint time |
| `df.stack(…)` | Pivots column level to row index |
| `df.unstack(…)` | Pivots row index to column level |
| `df.apply(fn, …)` | Output depends on the return type of `fn` |
| `df.map(fn, …)` | Output depends on `fn` |
| `df.transform(fn, …)` | Output depends on `fn` |
| `df.groupby(…).agg(…)` | Output columns are determined by aggregation spec |
| `df.with_columns(…)` | polars column addition/mutation; schema not narrowed statically |

---

## Error Code Reference

| Code | Severity | Message | Default |
|------|----------|---------|---------|
| `unknown-column` | Error | Column `'<name>'` not found in `<Schema>`. Did you mean `'<suggestion>'`? | Always reported |
| `reserved-name` | Error | Renamed-from column `'<name>'` not found in `<Schema>` | Always reported |
| `untracked-dataframe` | Warning | Columns unknown at lint time — annotate with a schema to enable column checking | Always reported |
| `dropped-unknown-column` | Warning | Dropped column `'<name>'` does not exist in `<Schema>` | Always reported |

**untracked-dataframe** downgrades to a quiet info-level note when `--lenient-ingest` is passed to the
CLI, for exploratory scripts that load data without a schema annotation and don't want the noise yet.

**unknown-column** reports the closest column name as a typo suggestion when the edit distance is
small (≤ 2 characters), which helps catch common capitalization and spelling mistakes.

# AST Method Matrix

This page documents how the `typedframes` static checker handles each DataFrame operation.
Operations fall into three categories: **schema-modifying** (the checker updates its
internal column model), **row-passthrough** (the checker assumes the schema is unchanged),
and **untracked** (the variable is dropped from tracking to avoid false positives).

Operations are matched by method name against a receiver the checker already tracks, so
the tables below apply to whichever backend implements that method. `dask.dataframe`
(experimental) reuses the pandas spellings throughout — the dask-only entries are called
out where they appear.

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
| `left.merge(right, …)` | Unions both schemas | `merged = a.merge(b, on="id")` |
| `pd.concat([df1, df2], …)` | Unions both schemas | `combined = pd.concat([a, b])` |
| `dd.from_pandas(pdf, …)` | Carries the source frame's schema over | `ddf = dd.from_pandas(pdf, npartitions=2)` |
| `pl.from_pandas(pdf)` | Same | `df = pl.from_pandas(pdf)` |

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
| `lf.collect()` | polars lazy → eager; columns unchanged |
| `ddf.compute()` | dask lazy → eager (a real pandas DataFrame); columns unchanged |
| `ddf.persist()` | dask; materializes into memory, columns unchanged |
| `ddf.repartition(…)` | dask; changes partition layout only, columns unchanged |

A chain is followed through named intermediates (`step = df.query(…)`, then
`out = step.sort_values(…)`), not through several calls stacked into a single
expression (`df.query(…).sort_values(…)`) — for every backend.

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
| `pd.merge(left, right, …)` | Module-level form; only the `left.merge(right, …)` instance form is tracked |
| `dd.from_delayed(…)` / `dd.from_array(…)` | dask; columns exist only once the graph runs |
| `dd.from_dict({…})` | dask; a literal rather than a tracked frame, like `pd.DataFrame({…})` |
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

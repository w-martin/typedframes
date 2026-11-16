# AST Method Matrix

This page documents how the `typedframes` static checker handles each DataFrame operation.
Operations fall into three categories: **schema-modifying** (the checker updates its
internal column model), **row-passthrough** (the checker assumes the schema is unchanged),
and **untracked** (the variable is dropped from tracking to avoid false positives).

---

## Schema-Modifying Operations

The checker updates its column model when it sees these operations, so subsequent accesses
are validated against the new schema.

| Operation | Effect on schema | Example |
|-----------|-----------------|---------|
| `df["col"] = val` | Adds `"col"` to an inferred schema (also `df[["a", "b"]] = …`, `df.loc[mask, "c"] = …`); a declared `BaseSchema` reports a column it does not list | `df["score"] = df["value"] * 2` |
| `del df["col"]` | Removes `"col"` from the schema | `del df["temp"]` |
| `df.drop(columns=[…])` | Removes listed columns | `df.drop(columns=["a", "b"])` |
| `df.drop([…], axis=1)` | Removes listed columns; without `axis=1`/`columns=` pandas drops rows, and with `level=` the columns are left as they were | `df.drop(["a", "b"], axis=1)` |
| `df.assign(col=…)` | Adds new column(s) to the schema | `df.assign(full_name=…)` |
| `df.rename(columns={…})` | Renames columns in the schema; a dict comprehension over `df.columns` with fixed string steps is evaluated | `df.rename(columns={"a": "b"})` |
| `df.columns = […]` | Replaces the columns; a list comprehension over `df.columns` with fixed string steps is evaluated | `df.columns = [c.lower() for c in df.columns]` |
| `df.with_columns(…)` (polars) | Adds the columns the keywords, `.alias(...)` and bare `pl.col(...)` chains name, also inside a list, a comprehension over a literal list, or a loop over one (`alias(f"{c}_x")`, `alias("g_" + g)`); an unreadable output name leaves the columns unknown | `df.with_columns(total=pl.col("a") * 2)` |
| `df.group_by(keys).agg(…)` (polars) | Keys followed by the aggregates' output names | `df.group_by("card").agg(s=pl.col("x").sum())` |
| `df.join(other, on=…)` (polars) | Left columns plus the right's non-key columns (overlaps get `suffix`, default `_right`); `inner`, `left`, `semi`, `anti` | `a.join(b, on="id")` |
| `df.select([…])` | Narrows schema to selected columns | `df.select(["id", "name"])` |
| `df.select(pl.col("…"))` | Narrows schema to the named column | `df.select(pl.col("id"))` |
| `df.pop("col")` | Removes `"col"` from the schema | `df.pop("score")` |
| `df.insert(pos, "col", val)` | Adds `"col"` to the schema | `df.insert(0, "rank", …)` |
| `df[["c1", "c2"]]` | Narrows schema to selected columns | `subset = df[["id", "name"]]` |
| `pd.merge(left, right, …)` / `df.merge(other, …)` | Both sides' columns. A non-key column both sides carry becomes `<c>_x`/`<c>_y` (or the given `suffixes=`) and the bare name is gone; with no `on=`/`left_on=`/`right_on=`, every shared column is the key. Keys or `suffixes=` that cannot be read keep both spellings (`suffixes=` that cannot be read leaves the result unresolved). Either side may be a chain of literal edits (`feats[["a", "b"]]`, `x.copy()`) | `df = a.merge(b, on="id", suffixes=("", "_b"))` |
| `pd.concat([df1, df2, …], …)` | Unions every operand's columns. An operand the checker cannot see ends tracking of the result (no claim is made about its columns) | `combined = pd.concat([a, b])` |
| `df.copy()`, `df.astype(…)`, `df.drop_duplicates(…)`, `df.groupby(k).tail(n)`, `df.loc[mask]`, `df[mask]`, `df.iloc[a:b]` | Same columns, under any name (`b = a.copy()` keeps `b` tracked like `a = a.copy()`, and so does `a[a["x"] > 0].copy()`) | `b = a.loc[a["x"] > 0]` |
| `df.set_index("k")` | Removes `k` (unless `drop=False`) | `b = a.set_index("k")` |
| `df.groupby(keys).agg(out=("col", "fn"), …)` (pandas) | The output names; the keys as well with `as_index=False` or after `reset_index()`. Each source column must exist | `seg = df.groupby("s", as_index=False).agg(n=("x", "sum"))` |
| `df.melt(id_vars=…, var_name=…, value_name=…)` / `df.unpivot(index=…, variable_name=…, value_name=…)` | The id columns followed by the variable and value columns (`variable` and `value` unless named); the melted columns are gone. An id or name that is not a literal leaves the columns unknown | `long = wide.melt(id_vars="k")` |
| `df.rename({…})` / `df.drop("a")` / `df.select(…)` / `df.with_columns(…)` (polars) | As the pandas forms above; `select` gives exactly the columns it names, whatever the frame held. The positional `rename`/`drop` are read as polars only in a file that imports polars and not pandas | `df.rename({"a": "b"}).select("b")` |
| `df.group_by("k").agg(…)` / `a.join(b, on=…)` (polars) | As above, anywhere in a chain, and also when the frame is a parameter: the result is the keys and aggregates, or the two sides' columns, whatever the input held | `e.group_by("id").agg(n=pl.len())` |
| `df.groupby(keys)["v"].agg(["mean", "sum"])` (pandas) | One column per named function; the keys as well after `reset_index()`. A function list on a whole frame, or `agg({"v": [...]})`, gives MultiIndex columns and is untracked | `s = df.groupby("k")["v"].agg(["min", "max"])` |
| `df.groupby(keys)["v"].nunique().reset_index(name="n")`, `df.groupby(keys).size().reset_index(name="n")` | The keys, then the named column | `n = df.groupby("k").size().reset_index(name="n")` |
| `series.to_frame()` / `to_frame("name")` | One column: the given name, or the name of `df["v"]` kept through a groupby or resample reduction and elementwise methods; the grouping keys stay index levels | `df.set_index("t").resample("D")["v"].sum().to_frame()` |
| `df.eval("c = a + b")` | Adds the assigned names (an `eval` without an assignment returns a Series) | `df.eval("c = a + b")` |
| `df.reset_index()` after `set_index`/a keyed aggregation | The index levels come back as columns | `df.set_index("k").reset_index()` |
| `df.pivot_table(index=…, values=…)` (pandas, no `columns=`) | The `values`; the index names join them after `reset_index()` | `rep = df.pivot_table(index="p", values=["a"])` |

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
| `df.join(other, …)` | (pandas, or polars with a `how=` other than the above) Output schema depends on join keys and `how=` parameter |
| `df.pivot(…)` | Output columns are derived from cell values at runtime |
| `df.pivot_table(…)` | Same as pivot, unless the `values`/`index` are literal and there is no `columns=` (above) |
| `df.melt(…)` | (with a non-literal `id_vars=`, `var_name=` or `value_name=`) the output schema depends on them; a literal call is tracked (above) |
| `df.explode(col)` | Schema depends on list column depth |
| `pd.get_dummies(df, …)` | Columns come from categorical values, unknown at lint time. With a literal `columns=[…]` the result is a frame those names are certainly gone from (see below) |
| `df.stack(…)` | Pivots column level to row index |
| `df.unstack(…)` | Pivots row index to column level |
| `df.apply(fn, …)` | Output depends on the return type of `fn` |
| `df.map(fn, …)` | Output depends on `fn` |
| `df.transform(fn, …)` | Output depends on `fn` |
| `df.groupby(…).agg(…)` | (pandas) Output columns are determined by aggregation spec, unless every aggregate is a literal `out=("col", "fn")` or a one-column function list (above) |

---

## Column names passed as arguments

A name given to one of these is checked against the frame the method is called on, wherever the
call sits in an expression: `merge` (`on=` against both sides, `left_on=` against the left,
`right_on=` against the right, also `pd.merge(left, right, …)`), `sort_values`, `drop_duplicates`
(`subset=`), `groupby`, and the polars `sort`, `unique(subset=)`, `group_by`, `select`,
`with_columns`, `filter`, `rename` and `join`. The bare names in a `query`/`eval` string are checked
the same way (not `@locals`, backticked names are, and a call with `local_dict=`, `global_dict=` or
`resolvers=` is skipped). The frame may be a name, a chain of literal edits, a
load call with `usecols=`/`columns=`, or a call to a function or method of the file whose result
columns are known. A name that may be an index level (the frame came through `set_index`, a
`groupby(...).agg(...)` without `reset_index()`, `pivot_table`, `stack`/`unstack`, `rename_axis`, a
constructor `index=` that may be named, or `df.index.name = …`) is not checked.

Reads are found in every expression: `assert` tests and messages, conditional expressions, dict and
set values, f-strings, `yield`, comprehensions and lambdas (unless their variable hides the frame's
name), and the target and value of `+=`.

## Frames known only by name (open frames)

A frame from a bare `pd.read_csv(...)`, an unresolved helper, or a `pd.DataFrame`-annotated
parameter has no column list, so most reads cannot be checked. The checker still follows what is
*certainly* gone from it, and reports a read of such a column as `unknown-column`:

| Edit | Effect |
|------|--------|
| `df.drop(columns=[…])` / `df = df.drop(…)` / `df.drop(…, inplace=True)` / `df.pop("c")` / `del df["c"]` | the names are gone |
| `df = df.rename(columns={"a": "b"})` | `a` is gone (and `b` is back if it had been dropped) |
| `pd.get_dummies(df, columns=["c"])` | `c` is gone |
| `df["c"] = …`, `df.assign(c=…)`, `df.insert(i, "c", …)` | `c` is back |

Only an edit that runs on every path counts (not one under an `if`, loop or `try`), a frame that is
rebuilt (`merge`, `concat`, a call the checker cannot see into) or handed to a function the checker
cannot see into starts over, and the copy a chain makes (`df.copy().drop(...)`, `df[mask]`,
`df.sort_values(...)`, polars `with_columns`) carries the facts along. A read in the same chain
(`frame.drop("a").select("a")`) is checked too. The facts do not survive a merge with another frame
whose columns are unknown, since that side may bring the column back.

A parameter annotated with a bare `pd.DataFrame`/`pl.DataFrame` is such a frame inside its function.
Columns are not propagated from a call site into the function: one function has many callers, and
the checker reports only what the function's own code makes certain. The other direction is checked:
a frame with known columns passed to a helper (by position or keyword) must have the columns that
helper reads on every path before changing the frame (see the usage guide's parameter contracts).

A column that is *read* before the only statement that assigns it, on such a frame, raises the
warning `column-read-before-assignment`; it is a warning because the data may already have the
column. A statement that reads the column it assigns (`df["x"] = df["x"].fillna(0)`) shows the column
was there, and silences it.

---

## Error Code Reference

| Code | Severity | Message | Default |
|------|----------|---------|---------|
| `unknown-column` | Error | Column `'<name>'` not found in `<Schema>`. Did you mean `'<suggestion>'`? | Always reported |
| `reserved-name` | Error | Renamed-from column `'<name>'` not found in `<Schema>` | Always reported |
| `untracked-dataframe` | Warning | Columns unknown at lint time — annotate with a schema to enable column checking | Always reported |
| `dropped-unknown-column` | Warning | Dropped column `'<name>'` does not exist in `<Schema>` | Always reported |
| `unverifiable-column` | Warning | Column `'<name>'` cannot be verified: the frame has no known column list | Once per frame; `--strict` fails on it |
| `column-read-before-assignment` | Warning | Column `'<name>'` is read before the only statement that gives the frame it | Always reported |
| `likely-column-typo` | Warning | Column `'<name>'` is read here and named nowhere else, but the code does name `'<other>'` | Always reported, on frames whose columns are unknown |
| `assumed-constant` | Info | `self.<NAME>` is read as a constant, assuming no subclass in another file overrides it; declare `<NAME>: Final = ...` to make that certain | Collapsed to one summary line per run unless `--verbose`; hidden by `--no-info`. `typedframes suggest` adds the `Final` (and its import) for a constant assigned once in a top-level class of the same file |

**untracked-dataframe** downgrades to a quiet info-level note when `--lenient-ingest` is passed to the
CLI, for exploratory scripts that load data without a schema annotation and don't want the noise yet.

**unknown-column** reports the closest column name as a typo suggestion when the edit distance is
small (≤ 2 characters), which helps catch common capitalization and spelling mistakes.

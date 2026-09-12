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

### PySpark (Experimental)

Spark spells most of these differently, so it gets its own names rather than reusing the
pandas/polars ones. See
[Native PySpark DataFrames](https://github.com/w-martin/typedframes#native-pyspark-dataframes-experimental).

| Operation | Effect on schema | Example |
|-----------|-----------------|---------|
| `df.withColumn("c", expr)` | Adds `"c"` (or replaces it) — Spark's `assign` | `df.withColumn("tax", F.col("amount") * 0.2)` |
| `df.withColumns({…})` | Adds/replaces each key | `df.withColumns({"tax": F.col("amount")})` |
| `df.withColumnRenamed(old, new)` | Renames `old` to `new`; the two names are separate positional arguments | `df.withColumnRenamed("amount", "total")` |
| `df.withColumnsRenamed({…})` | Renames by mapping | `df.withColumnsRenamed({"amount": "total"})` |
| `df.select(*cols)` | Narrows to the named columns; `.alias("x")` sets the output name | `df.select("id", F.col("amount").alias("total"))` |
| `df.drop(*cols)` | Removes listed columns — bare varargs, no `columns=` | `df.drop("region", "amount")` |
| `df.toDF(*names)` | Renames every column positionally | `df.toDF("a", "b")` |
| `df.union(other)` / `df.unionAll(other)` | Takes the LEFT schema — Spark resolves these by position | `left.union(right)` |
| `df.unionByName(other)` | Takes the left schema | `left.unionByName(right)` |
| `df.unionByName(other, allowMissingColumns=True)` | Unions both schemas | `left.unionByName(right, allowMissingColumns=True)` |
| `spark.read.schema(…).csv(path)` | Schema from a DDL string, `StructType`, or list of names (inline or in a variable) | `spark.read.schema("id INT").csv("f.csv")` |
| `spark.read.csv(path).select(…)` | Schema from the chained `select` | `spark.read.csv("f.csv").select("id")` |
| `spark.createDataFrame(data, schema)` | Schema from the second argument | `spark.createDataFrame(rows, ["id", "name"])` |
| `spark.sql(sql)` | Schema from the `SELECT` list, kept as a Spark DataFrame | `spark.sql("SELECT id FROM t")` |

A `spark.read` with no declared schema and no chained `select` reports
`untracked-dataframe`: Spark resolves that schema at runtime, so it is genuinely unknown
at lint time — the same situation as a bare `pd.read_csv()`.

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

### PySpark (Experimental)

| Operation | Notes |
|-----------|-------|
| `df.where(…)` | Alias of `filter`; columns unchanged |
| `df.limit(n)` / `df.offset(n)` | Row slice; columns unchanged |
| `df.orderBy(…)` / `df.sortWithinPartitions(…)` | Row sort; columns unchanged |
| `df.distinct()` | Row dedup; columns unchanged |
| `df.dropDuplicates(…)` / `df.dropDuplicatesWithinWatermark(…)` | Row dedup; columns unchanged |
| `df.repartition(…)` / `df.repartitionByRange(…)` / `df.coalesce(n)` | Partitioning only; columns unchanged |
| `df.cache()` / `df.persist()` / `df.unpersist()` | Caching only; columns unchanged |
| `df.checkpoint()` / `df.localCheckpoint()` | Checkpointing only; columns unchanged |
| `df.alias(name)` / `df.hint(…)` | Naming/planner hint; columns unchanged |

Spark's `head`, `tail` and `first` are **not** listed: they return `Row`/`list[Row]`
rather than a DataFrame. They are nonetheless treated as row-passthrough, because
`head`/`tail` are pandas methods that do return a frame and the checker tracks a
variable's column set rather than which library produced it.

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
| `df.join(other, …)` (PySpark) | `left_semi`/`left_anti` return only the left columns; `on=` as a name list collapses the join keys while a condition keeps both; duplicate names across the sides resolve at runtime |
| `df.selectExpr(…)` | Output names come from SQL expression strings, not parsed |
| `df.withColumn("c", udf(…))` | A UDF's output is opaque; the column is still added, but only when its name is a literal |
| `df.rdd.map(…)` / `df.mapInPandas(…)` / `df.mapInArrow(…)` | Output schema is decided by the function |
| `df.groupBy(…).agg(…)` (PySpark) | Output columns are determined by the aggregation spec |

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

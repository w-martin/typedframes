# Backends

Column tracking through a DataFrame library's own native API — the library's real
method names and its own idiom for referring to a column, rather than a SQL `SELECT`
list (`sql_connectors/`) or pandas/polars `usecols=`/`columns=` (`features/`).

Nothing in this group is executed, and none of these libraries need to be installed for
`typedframes check` to work on code that uses them: the checker reads the source.

| Directory | Backend | Status |
|---|---|---|
| [`pyspark/`](pyspark/) | `pyspark.sql.DataFrame` | Experimental |

## pyspark/

Native `pyspark.sql.DataFrame` tracking: `spark.read` / `createDataFrame` /
`spark.sql` kept as a Spark DataFrame and chained through `select`, `withColumn`,
`withColumnRenamed`, `drop`, and `union`, with `F.col("...")` references validated the
way `pl.col("...")` already is.

This is a different mechanism from [`sql_connectors/pyspark/`](../sql_connectors/pyspark/),
which covers `spark.sql(sql).toPandas()` — there the frame is converted to *pandas* and
the columns come from the query's `SELECT` list. Both work, and both are shown in
`example.py`.

```shell
uv run typedframes check examples/backends/pyspark/
```

Three diagnostics, all deliberate: one `untracked-dataframe` warning for a read whose
schema Spark only resolves at runtime, and two `unknown-column` errors — a plain
misspelled column, and a `withColumnRenamed` of a column that does not exist (which
Spark documents as a silent no-op, so nothing at runtime would tell you either).

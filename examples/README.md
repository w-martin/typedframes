# Examples

Three groups, each with its own README/SPEC:

## [`features/`](features/)

Core typedframes usage: inference from `usecols=`/`columns=`, multi-file schema
propagation, `Annotated` schemas for pandas/polars, schema algebra, and the Pandera
bridge. Start here if you're new to typedframes. See
[`features/README.md`](features/README.md).

## [`sql_connectors/`](sql_connectors/)

SQL / data-warehouse column inference — the column set is inferred from a query's
`SELECT` list instead of `usecols=`/`columns=`, with dialect-aware identifier case
folding. One directory per connector (Snowflake, BigQuery, Athena, Redshift,
Databricks, PySpark, DuckDB, connectorx, SQLAlchemy, Feast, Azure Synapse/Fabric),
each a self-contained, runnable environment. See
[`sql_connectors/README.md`](sql_connectors/README.md).

## [`comparisons/`](comparisons/)

Side-by-side comparisons against other dataframe-validation libraries (dataenforce,
dataframely, great_expectations, narwhals, pandas-stubs, pandera, patito,
static_frame, strictly_typed_pandas) plus a plain pandas-stubs/mypy-only baseline.
Each directory has its own `SPEC.md` write-up and a runnable environment
(`pyproject.toml` + `uv.lock`).

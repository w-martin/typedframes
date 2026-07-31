"""SQL column inference against Databricks.

Databricks/Spark SQL preserves the source case of unquoted identifiers by default
(case-sensitivity is configurable per-session via `spark.sql.caseSensitive`, but the
default is off) — `sql_dialect = "databricks"` doesn't fold case, same as leaving it
unset, but is set here for documentation/consistency with the other examples.

Run: `typedframes check examples/sql_connectors/databricks/`
Run for real: `docker compose up -d && uv run python example.py; docker compose down`

NOT independently verified end-to-end (no docker/JVM available in the environment this
was written in) -- of everything in this repo, this one carries the most uncertainty:
real `databricks-connect` needs an actual Databricks workspace and has no OSS/local
equivalent, so `databricks_session_shim.py` substitutes a `DatabricksSession`-shaped
wrapper around real, open-source Apache Spark's own Spark Connect protocol (which
`databricks-connect` is itself built on) -- a real local Spark Connect *server* (started
by docker-compose, from the official `apache/spark` image) behind a fake client-side
class. The `databricks-sql-connector` path (`load_via_sql_connector`) has no such
stand-in available at all -- it speaks Databricks' own proprietary Thrift SQL-warehouse
protocol -- so that one is left un-run, real package, real import, no real endpoint.
"""

import os

import pandas as pd
from databricks import sql
from databricks_session_shim import DatabricksSession


def load_via_databricks_connect() -> None:
    """The modern, Spark-native pattern: session.sql(sql).toPandas().

    Runs SQL from a local Python process against a Databricks cluster via
    databricks-connect.
    """
    session = DatabricksSession.builder.host("workspace-url").create()
    df = session.sql(
        "SELECT customer_id, order_date, amount FROM customers.orders WHERE status = 'completed'"
    ).toPandas()

    print(df["customer_id"])  # OK
    print(df["amount"])  # OK
    # print(df["status"])  -- unknown-column: not in {customer_id, order_date, amount}


def load_via_sql_connector() -> None:
    """databricks-sql-connector implements PEP 249.

    So pd.read_sql(sql, conn) with the SQL text as the first positional argument works
    the same as any other DB-API connection.
    """
    conn = sql.connect(
        server_hostname="...",
        http_path="...",
        access_token=os.environ["DATABRICKS_TOKEN"],
    )
    df = pd.read_sql("SELECT order_id, amount FROM orders", conn)

    print(df["order_id"])  # OK
    # print(df["status"])  -- unknown-column: not in {order_id, amount}


def build_orders_query(catalog: str) -> str:
    """A query built at runtime -- not a literal, so the checker can't resolve it."""
    return f"SELECT * FROM {catalog}.customers.orders"


def load_with_dynamic_catalog(catalog: str) -> None:
    """A query assembled from a dynamic catalog/schema — deliberately unresolved."""
    session = DatabricksSession.builder.host("workspace-url").create()
    query = build_orders_query(catalog)
    df = session.sql(query).toPandas()  # untracked-dataframe: not a literal
    print(df)


def _seed_customers_orders(session: object) -> None:
    """Create the `customers.orders` table the demo queries -- not part of the checker demo."""
    session.sql("CREATE SCHEMA IF NOT EXISTS customers")
    seed_df = session.createDataFrame(
        [
            (101, "2026-01-05", 150.00, "completed"),
            (102, "2026-01-06", 89.50, "pending"),
        ],
        ["customer_id", "order_date", "amount", "status"],
    )
    seed_df.write.mode("overwrite").saveAsTable("customers.orders")


def load_with_unknown_column() -> None:
    """A real bug: 'status' is not in the inferred column set.

    Left out of __main__ below -- this is a static-analysis fixture, not meant to be
    executed. Run `typedframes check .` to see it caught as unknown-column.
    """
    session = DatabricksSession.builder.host("workspace-url").create()
    df = session.sql("SELECT customer_id, order_date, amount FROM customers.orders WHERE status = 'completed'").toPandas()
    print(df["status"])  # unknown-column: not in {customer_id, order_date, amount}


if __name__ == "__main__":
    session = DatabricksSession.builder.host("workspace-url").create()
    _seed_customers_orders(session)
    load_via_databricks_connect()
    load_with_dynamic_catalog("main")
    # load_via_sql_connector() is intentionally not run here -- see the module
    # docstring: databricks-sql-connector has no local/OSS stand-in at all.
    # load_with_unknown_column() intentionally not run -- see its docstring.

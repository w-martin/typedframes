"""SQL column inference against Databricks.

Databricks/Spark SQL preserves the source case of unquoted identifiers by default
(case-sensitivity is configurable per-session via `spark.sql.caseSensitive`, but the
default is off) — `sql_dialect = "databricks"` doesn't fold case, same as leaving it
unset, but is set here for documentation/consistency with the other examples.

Run: `typedframes check examples/databricks/`
"""

import os

import pandas as pd


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


def load_with_dynamic_catalog(catalog: str) -> None:
    """A query assembled from a dynamic catalog/schema — deliberately unresolved."""
    session = DatabricksSession.builder.host("workspace-url").create()
    query = build_orders_query(catalog)
    df = session.sql(query).toPandas()  # untracked-dataframe: not a literal
    print(df)

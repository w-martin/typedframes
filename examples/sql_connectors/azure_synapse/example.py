"""SQL column inference against Azure Synapse Analytics / Azure SQL / Fabric Warehouse.

Azure's closest analog to AWS Athena — serverless SQL over data-lake files — is Azure
Synapse's serverless SQL pool (or, in Microsoft Fabric, a Warehouse's SQL endpoint).
Both speak T-SQL and are reached the same way as any other pyodbc/SQLAlchemy
connection, so `pd.read_sql(sql, conn)` already works with no special-casing. The one
thing that DID need a fix is T-SQL's `[bracket-quoted]` identifier convention — what
SSMS and most ORM-generated SQL Server/Synapse queries actually use — which the
checker's grammar didn't accept at all until now.

`sql_dialect = "synapse"` (also: `mssql`, `sqlserver`, `fabric`) doesn't fold case —
SQL Server preserves the case an unquoted identifier was declared with rather than
rewriting it, the same as leaving `sql_dialect` unset — but is set here so the choice
is explicit rather than an unrecognized-value coincidence.

Run: `typedframes check examples/sql_connectors/azure_synapse/`
Run for real: `docker compose up -d && docker compose run --rm app; docker compose down`

NOT independently verified end-to-end (no docker available in the environment this was
written in) -- the docker-compose/SQL Server + msodbcsql18 setup should work, but hasn't
actually been run against a live container.
"""

from pathlib import Path
from typing import TYPE_CHECKING

import pandas as pd
import pyodbc

if TYPE_CHECKING:
    from pyspark.sql import SparkSession

CONN_STR = (
    # "mssql" is docker-compose's service name for the SQL Server container, reachable
    # by that name from the "app" service's container on the compose network. Running
    # example.py from the host instead (rather than via `docker compose run app`) would
    # need "localhost,1433" here and a local ODBC driver install.
    "Driver={ODBC Driver 18 for SQL Server};Server=tcp:mssql,1433;"
    "Database=master;UID=sa;PWD=yourStrong(!)Password;TrustServerCertificate=yes"
)


def load_via_pyodbc() -> None:
    """Pyodbc (or SQLAlchemy's mssql+pyodbc driver) + pd.read_sql — plain identifiers.

    Real Synapse/Fabric code connects with
    "Server=tcp:my-workspace.sql.azuresynapse.net;Authentication=ActiveDirectoryInteractive"
    -- CONN_STR here points at the local docker-compose SQL Server instead so this
    function can actually run.
    """
    conn = pyodbc.connect(CONN_STR)
    df = pd.read_sql("SELECT order_id, customer_id, amount FROM dbo.orders", conn)

    print(df["order_id"])  # OK
    # print(df["revenue"])  -- unknown-column: not in {order_id, customer_id, amount}


def load_with_bracket_quoted_identifiers() -> None:
    """T-SQL's [bracket] quoting — SSMS's default identifier style.

    Also what most ORM-generated or hand-written SQL Server/Synapse queries use.
    """
    conn = pyodbc.connect(CONN_STR)
    df = pd.read_sql("SELECT [OrderID], [CustomerID], [Amount] FROM [dbo].[Orders]", conn)

    print(df["OrderID"])  # OK -- bracket quoting is preserved verbatim, like "double quotes"
    print(df["CustomerID"])  # OK
    # print(df["Revenue"])  -- unknown-column: not in {OrderID, CustomerID, Amount}


def load_via_query_file() -> None:
    """A query kept in a .sql file alongside this one."""
    sql = (Path(__file__).parent / "orders.sql").read_text()
    conn = pyodbc.connect(CONN_STR)
    df = pd.read_sql(sql, conn)

    print(df["Status"])  # OK -- orders.sql selects [OrderID], [Amount], [Status]
    # print(df["CustomerID"])  -- unknown-column: not in {OrderID, Amount, Status}


def load_via_fabric_spark_notebook(spark: "SparkSession") -> None:
    """Microsoft Fabric / Synapse Spark notebooks are Spark-based.

    So spark.sql(sql).toPandas() — the same pattern as plain PySpark/Databricks —
    already works too. Not exercised by the `__main__` block below: this directory's
    docker-compose only provisions SQL Server, not a JVM/Spark runtime -- see
    examples/sql_connectors/pyspark/ for a fully worked, actually-run Spark example.
    """
    df = spark.sql("SELECT customer_id, order_date, amount FROM orders WHERE status = 'completed'").toPandas()

    print(df["customer_id"])  # OK
    # print(df["status"])  -- unknown-column: not in {customer_id, order_date, amount}


def build_orders_query(schema: str) -> str:
    """A query built at runtime -- not a literal, so the checker can't resolve it."""
    return f"SELECT * FROM [{schema}].[Orders]"


def load_with_dynamic_schema(schema: str) -> None:
    """A query assembled from a dynamic schema name — deliberately unresolved."""
    conn = pyodbc.connect(CONN_STR)
    query = build_orders_query(schema)
    df = pd.read_sql(query, conn)  # untracked-dataframe: not a literal
    print(df)


def load_with_unknown_column() -> None:
    """A real bug: 'Revenue' is not in the inferred column set.

    Left out of __main__ below -- this is a static-analysis fixture, not meant to be
    executed. Run `typedframes check .` to see it caught as unknown-column.
    """
    conn = pyodbc.connect(CONN_STR)
    df = pd.read_sql("SELECT [OrderID], [CustomerID], [Amount] FROM [dbo].[Orders]", conn)
    print(df["Revenue"])  # unknown-column: not in {OrderID, CustomerID, Amount}


# NOTE: Azure Data Explorer (Kusto) is NOT covered here — its query language, KQL, is
# not SQL at all, so the SQL-text parser this checker uses could never resolve columns
# from a KustoClient.execute(database, kql_query) call. That would need a wholly
# separate KQL-aware extractor; annotate the result explicitly if you use Kusto:
#
#     df: Annotated[pd.DataFrame, DriverStatsSchema] = dataframe_from_result_table(
#         client.execute(database, "DriverStats | project conv_rate, acc_rate").primary_results[0]
#     )


def _seed_database() -> None:
    """Drop/recreate dbo.Orders and insert fixture rows -- not part of the checker demo.

    SQL Server's default collation is case-insensitive for identifier resolution, so
    the lowercase `dbo.orders`/`order_id` spelling in load_via_pyodbc() still resolves
    against this table regardless of its declared (bracket-style) case.
    """
    conn = pyodbc.connect(CONN_STR, autocommit=True)
    cursor = conn.cursor()
    cursor.execute("IF OBJECT_ID('dbo.Orders', 'U') IS NOT NULL DROP TABLE dbo.Orders")
    cursor.execute(
        """
        CREATE TABLE dbo.Orders (
            OrderID INT PRIMARY KEY,
            CustomerID INT NOT NULL,
            Amount DECIMAL(10, 2) NOT NULL,
            Status VARCHAR(50) NOT NULL
        )
        """
    )
    cursor.execute(
        "INSERT INTO dbo.Orders (OrderID, CustomerID, Amount, Status) VALUES "
        "(1, 101, 150.00, 'completed'), (2, 102, 89.50, 'completed')"
    )
    conn.close()


if __name__ == "__main__":
    _seed_database()
    load_via_pyodbc()
    load_with_bracket_quoted_identifiers()
    load_via_query_file()
    load_with_dynamic_schema("dbo")
    # load_via_fabric_spark_notebook() is intentionally not run here -- see its
    # docstring: it needs a Spark runtime, which this directory's docker-compose
    # doesn't provision.
    # load_with_unknown_column() intentionally not run -- see its docstring.

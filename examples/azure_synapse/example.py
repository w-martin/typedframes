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

Run: `typedframes check examples/azure_synapse/`
"""

from pathlib import Path

import pandas as pd


def load_via_pyodbc() -> None:
    """Pyodbc (or SQLAlchemy's mssql+pyodbc driver) + pd.read_sql — plain identifiers."""
    conn = pyodbc.connect(
        "Driver={ODBC Driver 18 for SQL Server};"
        "Server=tcp:my-workspace.sql.azuresynapse.net;"
        "Database=mydb;Authentication=ActiveDirectoryInteractive"
    )
    df = pd.read_sql("SELECT order_id, customer_id, amount FROM dbo.orders", conn)

    print(df["order_id"])  # OK
    # print(df["revenue"])  -- unknown-column: not in {order_id, customer_id, amount}


def load_with_bracket_quoted_identifiers() -> None:
    """T-SQL's [bracket] quoting — SSMS's default identifier style.

    Also what most ORM-generated or hand-written SQL Server/Synapse queries use.
    """
    conn = pyodbc.connect("Driver={ODBC Driver 18 for SQL Server};...")
    df = pd.read_sql("SELECT [OrderID], [CustomerID], [Amount] FROM [dbo].[Orders]", conn)

    print(df["OrderID"])  # OK -- bracket quoting is preserved verbatim, like "double quotes"
    print(df["CustomerID"])  # OK
    # print(df["Revenue"])  -- unknown-column: not in {OrderID, CustomerID, Amount}


def load_via_query_file() -> None:
    """A query kept in a .sql file alongside this one."""
    sql = (Path(__file__).parent / "orders.sql").read_text()
    conn = pyodbc.connect("Driver={ODBC Driver 18 for SQL Server};...")
    df = pd.read_sql(sql, conn)

    print(df["Status"])  # OK -- orders.sql selects [OrderID], [Amount], [Status]
    # print(df["CustomerID"])  -- unknown-column: not in {OrderID, Amount, Status}


def load_via_fabric_spark_notebook() -> None:
    """Microsoft Fabric / Synapse Spark notebooks are Spark-based.

    So spark.sql(sql).toPandas() — the same pattern as plain PySpark/Databricks —
    already works too.
    """
    df = spark.sql("SELECT customer_id, order_date, amount FROM orders WHERE status = 'completed'").toPandas()

    print(df["customer_id"])  # OK
    # print(df["status"])  -- unknown-column: not in {customer_id, order_date, amount}


# NOTE: Azure Data Explorer (Kusto) is NOT covered here — its query language, KQL, is
# not SQL at all, so the SQL-text parser this checker uses could never resolve columns
# from a KustoClient.execute(database, kql_query) call. That would need a wholly
# separate KQL-aware extractor; annotate the result explicitly if you use Kusto:
#
#     df: Annotated[pd.DataFrame, DriverStatsSchema] = dataframe_from_result_table(
#         client.execute(database, "DriverStats | project conv_rate, acc_rate").primary_results[0]
#     )

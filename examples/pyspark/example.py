"""SQL column inference against Apache Spark (PySpark).

Spark SQL preserves unquoted-identifier case by default, so `sql_dialect` is left unset
(Generic) in this directory.

Run: `typedframes check examples/pyspark/`
"""

from pathlib import Path


def load_via_spark_sql() -> None:
    """The standard PySpark pattern: SparkSession.sql(sql).toPandas()."""
    spark = SparkSession.builder.appName("orders").getOrCreate()
    df = spark.sql("SELECT customer_id, order_date, amount FROM orders WHERE order_year > 2023").toPandas()

    print(df["customer_id"])  # OK
    print(df["order_date"])  # OK
    # print(df["region"])  -- unknown-column: not in {customer_id, order_date, amount}


def load_via_query_file() -> None:
    """A query kept in a .sql file alongside this one."""
    spark = SparkSession.builder.appName("orders").getOrCreate()
    sql = (Path(__file__).parent / "orders.sql").read_text()
    df = spark.sql(sql).toPandas()

    print(df["amount"])  # OK -- orders.sql selects customer_id, amount, status
    # print(df["order_date"])  -- unknown-column: not in {customer_id, amount, status}

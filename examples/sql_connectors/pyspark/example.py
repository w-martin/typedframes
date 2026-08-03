"""SQL column inference against Apache Spark (PySpark).

Spark SQL preserves unquoted-identifier case by default, so `sql_dialect` is left unset
(Generic) in this directory.

Run: `typedframes check examples/sql_connectors/pyspark/`
Run for real: `docker compose run --rm app` (needs a JDK; the compose service provides one)

NOT independently verified end-to-end (no JVM/docker available in the environment this
was written in) -- `uv sync` and `typedframes check` both pass; the actual Spark
session/query execution hasn't been run.
"""

from pathlib import Path

from pyspark.sql import Row, SparkSession


def _seed_orders(spark: SparkSession) -> None:
    """Register an in-memory `orders` table -- not part of the checker demo."""
    rows = [
        Row(customer_id=101, order_date="2026-01-05", amount=150.00, status="completed", order_year=2026),
        Row(customer_id=102, order_date="2026-01-06", amount=89.50, status="completed", order_year=2026),
    ]
    spark.createDataFrame(rows).createOrReplaceTempView("orders")


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


def build_year_filtered_query(year: int) -> str:
    return f"SELECT customer_id, order_date, amount FROM orders WHERE order_year > {year}"


def load_with_dynamic_year(year: int) -> None:
    """A query built from a runtime year — deliberately unresolved."""
    spark = SparkSession.builder.appName("orders").getOrCreate()
    query = build_year_filtered_query(year)
    df = spark.sql(query).toPandas()  # untracked-dataframe: not a literal
    print(df)


def load_with_unknown_column() -> None:
    """A real bug: 'region' is not in the inferred column set.

    Left out of __main__ below -- this is a static-analysis fixture, not meant to be
    executed. Run `typedframes check .` to see it caught as unknown-column.
    """
    spark = SparkSession.builder.appName("orders").getOrCreate()
    df = spark.sql("SELECT customer_id, order_date, amount FROM orders WHERE order_year > 2023").toPandas()
    print(df["region"])  # unknown-column: not in {customer_id, order_date, amount}


if __name__ == "__main__":
    _seed_orders(SparkSession.builder.appName("orders").getOrCreate())
    load_via_spark_sql()
    load_via_query_file()
    load_with_dynamic_year(2023)
    # load_with_unknown_column() intentionally not run -- see its docstring.

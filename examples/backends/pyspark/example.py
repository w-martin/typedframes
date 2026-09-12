"""PySpark example: native `pyspark.sql.DataFrame` tracking (experimental).

This is the NATIVE Spark API path -- `spark.read` / `createDataFrame` / `spark.sql`
kept as a Spark DataFrame and chained through `select` / `withColumn` /
`withColumnRenamed` / `drop` / `union`. It is a different mechanism from
`examples/sql_connectors/pyspark/`, which covers `spark.sql(sql).toPandas()`: there the
frame is converted to *pandas* and the columns come from the SQL `SELECT` list. Both
work, and they coexist -- see `load_via_spark_sql_kept_native()` below.

This file spells the functions module `from pyspark.sql import functions` rather than
the more common `from pyspark.sql import functions as F`, only because the aliased form
trips ruff's N812 (lowercase imported as non-lowercase) and this repo doesn't carry a
lint-ignore for it. The checker treats `functions.col(...)`, `F.col(...)`, `sf.col(...)`
and a bare `from pyspark.sql.functions import col` identically -- use whichever your
codebase prefers.

Nothing here is executed, and `pyspark` does not need to be installed to check it: the
checker reads the source, so `typedframes check` works on a machine with no Spark and no
JVM. Run it with:

    uv run typedframes check examples/backends/pyspark/

Spark's column set is knowable at lint time in three ways, all shown below: an explicit
schema, a `.select(...)` naming the columns, or a `SELECT` list. A bare
`spark.read.csv(path)` is none of those -- Spark decides that schema at runtime from the
data -- so it reports `untracked-dataframe`, exactly as a bare `pd.read_csv()` does.
"""

from typing import Annotated

import pyspark
from pyspark.sql import functions
from pyspark.sql.types import DoubleType, IntegerType, StringType, StructField, StructType

from typedframes import BaseSchema, Column

ORDERS_DDL = "order_id INT, customer_id INT, amount DOUBLE, region STRING"


class OrderSchema(BaseSchema):
    """Schema for the orders dataset."""

    order_id = Column(type=int)
    customer_id = Column(type=int)
    amount = Column(type=float)


def load_with_declared_schema(spark: pyspark.sql.SparkSession) -> None:
    """A DDL schema string on the reader chain names the columns outright."""
    orders = spark.read.schema("order_id INT, customer_id INT, amount DOUBLE, region STRING").csv("orders.csv")

    print(orders["region"])  # OK
    print(orders.amount)  # OK -- attribute access is checked too
    print(orders.select(functions.col("customer_id")))  # OK -- col() is Spark's pl.col()


def load_with_struct_type(spark: pyspark.sql.SparkSession) -> None:
    """A `StructType` held in a variable is resolved the same way as the DDL string."""
    schema = StructType(
        [
            StructField("order_id", IntegerType()),
            StructField("customer_id", IntegerType()),
            StructField("amount", DoubleType()),
            StructField("region", StringType()),
        ]
    )
    orders = spark.read.schema(schema).parquet("orders.parquet")

    print(orders["amount"])  # OK


def structural_operations(spark: pyspark.sql.SparkSession) -> None:
    """Spark's structural ops carry the column set forward, each in its own way."""
    orders = spark.read.schema(ORDERS_DDL).csv("orders.csv")

    # withColumn is the assign-equivalent: base columns plus the new one.
    with_tax = orders.withColumn("tax", functions.col("amount") * 0.2)
    print(with_tax["tax"])  # OK

    # withColumnRenamed takes the two names positionally, not a mapping.
    renamed = orders.withColumnRenamed("amount", "total")
    print(renamed["total"])  # OK

    # drop takes bare varargs, not a `columns=` keyword.
    trimmed = orders.drop("region", "customer_id")
    print(trimmed["order_id"])  # OK

    # An alias is the OUTPUT name; the aliased source column is checked separately.
    summary = orders.select(functions.col("order_id"), functions.col("amount").alias("total_amount"))
    print(summary["total_amount"])  # OK

    # Row-preserving ops leave the column set alone.
    recent = orders.filter(functions.col("amount") > 100).distinct()
    print(recent["region"])  # OK


def load_via_select_after_read(spark: pyspark.sql.SparkSession) -> None:
    """No declared schema, but a `.select(...)` naming the columns settles it."""
    orders = spark.read.csv("orders.csv").select("order_id", "amount")

    print(orders["amount"])  # OK


def load_via_spark_sql_kept_native(spark: pyspark.sql.SparkSession) -> None:
    """`spark.sql(...)` NOT converted to pandas -- columns from the SELECT list."""
    orders = spark.sql("SELECT order_id, customer_id, amount FROM orders")

    print(orders["customer_id"])  # OK


def load_with_annotation(spark: pyspark.sql.SparkSession) -> None:
    """`Annotated[pyspark.sql.DataFrame, Schema]`, the same as pandas and polars."""
    orders: Annotated[pyspark.sql.DataFrame, OrderSchema] = spark.read.csv("orders.csv")

    print(orders["amount"])  # OK


def load_with_inferred_schema(spark: pyspark.sql.SparkSession) -> None:
    """Deliberately unresolvable: Spark infers this schema at runtime from the data."""
    orders = spark.read.csv("orders.csv", header=True, inferSchema=True)  # untracked-dataframe
    print(orders)


def load_with_unknown_column(spark: pyspark.sql.SparkSession) -> None:
    """A real bug: 'revenue' is not in the declared schema.

    Left out of `main()` below -- this is a static-analysis fixture, not meant to be
    executed. Run `typedframes check .` to see it caught as unknown-column.
    """
    orders = spark.read.schema(ORDERS_DDL).csv("orders.csv")

    print(orders["revenue"])  # ✗ unknown-column: not in {order_id, customer_id, amount, region}


def rename_a_column_that_does_not_exist(spark: pyspark.sql.SparkSession) -> None:
    """A second real bug, and one Spark itself will not tell you about.

    `withColumnRenamed` is documented as a NO-OP when the column is absent, so this
    silently does nothing at runtime instead of raising. Also left out of `main()`.
    """
    orders = spark.read.schema(ORDERS_DDL).csv("orders.csv")
    renamed = orders.withColumnRenamed("revenue", "total")  # ✗ unknown-column (rename)

    print(renamed)


def main() -> None:
    """Run the cases that have no deliberate bug in them."""
    spark = pyspark.sql.SparkSession.builder.appName("orders").getOrCreate()

    load_with_declared_schema(spark)
    load_with_struct_type(spark)
    structural_operations(spark)
    load_via_select_after_read(spark)
    load_via_spark_sql_kept_native(spark)
    load_with_annotation(spark)
    load_with_inferred_schema(spark)


if __name__ == "__main__":
    main()

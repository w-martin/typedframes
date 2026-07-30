"""SQL column inference against AWS Athena.

Athena's Presto-derived engine is case-insensitive for unquoted identifiers, and this
directory intentionally leaves `sql_dialect` unset (Generic — no case folding, exact
match on whatever the query text says) since there's no dedicated Athena/Presto/Trino
folding rule to apply here.

Run: `typedframes check examples/athena/`
"""

from pathlib import Path

import pandas as pd


def load_via_pyathena() -> None:
    """Pyathena's connection/cursor implements PEP 249.

    So pd.read_sql(sql, conn) — with the SQL text as the first positional argument,
    same as any other DB-API connection — already works with no special-casing needed.
    """
    conn = connect(s3_staging_dir="s3://my-bucket/athena-results/", region_name="us-east-1")
    df = pd.read_sql("SELECT order_id, customer_id, amount FROM orders", conn)

    print(df["order_id"])  # OK
    # print(df["revenue"])  -- unknown-column: not in {order_id, customer_id, amount}


def load_via_query_file() -> None:
    """A query kept in a .sql file alongside this one."""
    sql = (Path(__file__).parent / "orders.sql").read_text()
    conn = connect(s3_staging_dir="s3://my-bucket/athena-results/", region_name="us-east-1")
    df = pd.read_sql(sql, conn)

    print(df["status"])  # OK -- orders.sql selects order_id, amount, status
    # print(df["customer_id"])  -- unknown-column: not in {order_id, amount, status}


# NOTE: awswrangler's wr.athena.read_sql_query(sql=..., database=..., s3_output=...) is
# also a common way to query Athena into a DataFrame. typedframes does not currently
# recognize it (the two-level `wr.athena.*` module path doesn't match the checker's
# receiver dispatch) — it falls through to untracked-dataframe like any other
# unrecognized call, rather than being incorrectly inferred. Prefer the pd.read_sql
# pattern above, or annotate the result explicitly, if you use awswrangler:
#
#     df: Annotated[pd.DataFrame, OrderSchema] = wr.athena.read_sql_query(
#         sql="SELECT order_id, amount FROM orders",
#         database="analytics",
#         s3_output="s3://my-bucket/athena-results/",
#     )

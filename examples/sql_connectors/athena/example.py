"""SQL column inference against AWS Athena.

Athena's Presto-derived engine is case-insensitive for unquoted identifiers, and this
directory intentionally leaves `sql_dialect` unset (Generic — no case folding, exact
match on whatever the query text says) since there's no dedicated Athena/Presto/Trino
folding rule to apply here.

Run: `typedframes check examples/sql_connectors/athena/`
Run for real: `uv run python example.py` (mocked AWS via moto, no docker/AWS account)
"""

from pathlib import Path

import pandas as pd
from pyathena import connect


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


def build_table_query(table: str) -> str:
    return f"SELECT * FROM {table}"


def load_with_dynamic_table(table: str) -> None:
    """A query built from a runtime table name — deliberately unresolved."""
    conn = connect(s3_staging_dir="s3://my-bucket/athena-results/", region_name="us-east-1")
    query = build_table_query(table)
    df = pd.read_sql(query, conn)  # untracked-dataframe: not a literal
    print(df)


def load_with_unknown_column() -> None:
    """A real bug: 'revenue' is not in the inferred column set.

    Left out of __main__ below -- this is a static-analysis fixture, not meant to be
    executed. Run `typedframes check .` to see it caught as unknown-column.
    """
    conn = connect(s3_staging_dir="s3://my-bucket/athena-results/", region_name="us-east-1")
    df = pd.read_sql("SELECT order_id, customer_id, amount FROM orders", conn)
    print(df["revenue"])  # unknown-column: not in {order_id, customer_id, amount}


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


if __name__ == "__main__":
    from moto import mock_aws
    from moto_setup import setup_moto_athena

    with mock_aws():
        setup_moto_athena()
        load_via_pyathena()
        load_via_query_file()
        load_with_dynamic_table("orders")
        # load_with_unknown_column() intentionally not run -- see its docstring.

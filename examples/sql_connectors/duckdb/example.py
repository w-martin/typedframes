"""SQL column inference against DuckDB.

DuckDB is case-insensitive but preserves source case for unquoted identifiers, so
`sql_dialect = "duckdb"` doesn't change column casing — set here for documentation/
consistency with the other examples.

Run: `typedframes check examples/sql_connectors/duckdb/`
Run for real: `cd examples/sql_connectors/duckdb && uv run python example.py`
"""

import os
from pathlib import Path

import duckdb


def load_via_duckdb_sql_pandas() -> None:
    """duckdb.sql(sql).df() — DuckDB's own pandas-materializing call."""
    df = duckdb.sql("SELECT customer_id, order_date, amount FROM 'orders.parquet' WHERE amount > 100").df()

    print(df["customer_id"])  # OK
    print(df["amount"])  # OK
    # print(df["region"])  -- unknown-column: not in {customer_id, order_date, amount}


def load_via_duckdb_sql_polars() -> None:
    """duckdb.sql(sql).pl() — the polars equivalent of .df()."""
    df = duckdb.sql("SELECT product_id, price FROM 'products.parquet'").pl()

    print(df["product_id"])  # OK
    # print(df["stock"])  -- unknown-column: not in {product_id, price}


def load_via_query_file() -> None:
    """A query kept in a .sql file alongside this one."""
    sql = (Path(__file__).parent / "orders.sql").read_text()
    df = duckdb.sql(sql).df()

    print(df["amount"])  # OK -- orders.sql selects order_id, amount
    # print(df["customer_id"])  -- unknown-column: not in {order_id, amount}


def build_table_query(table: str) -> str:
    return f"SELECT * FROM '{table}.parquet'"


def load_with_dynamic_table(table: str) -> None:
    """A query built from a runtime table name — deliberately unresolved."""
    query = build_table_query(table)
    df = duckdb.sql(query).df()  # untracked-dataframe: not a literal
    print(df)


def load_with_unknown_column() -> None:
    """A real bug: 'region' is not in the inferred column set.

    Left out of __main__ below -- this is a static-analysis fixture, not meant to be
    executed. Run `typedframes check .` to see it caught as unknown-column.
    """
    df = duckdb.sql("SELECT customer_id, order_date, amount FROM 'orders.parquet'").df()
    print(df["region"])  # unknown-column: not in {customer_id, order_date, amount}


if __name__ == "__main__":
    # duckdb.sql()'s FROM 'orders.parquet' is a relative path, resolved against the
    # process's cwd -- chdir so this runs the same regardless of where it's invoked from.
    os.chdir(Path(__file__).parent)
    load_via_duckdb_sql_pandas()
    load_via_duckdb_sql_polars()
    load_via_query_file()
    load_with_dynamic_table("orders")
    # load_with_unknown_column() intentionally not run -- see its docstring.

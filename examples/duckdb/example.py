"""SQL column inference against DuckDB.

DuckDB is case-insensitive but preserves source case for unquoted identifiers, so
`sql_dialect = "duckdb"` doesn't change column casing — set here for documentation/
consistency with the other examples.

Run: `typedframes check examples/duckdb/`
"""

from pathlib import Path


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

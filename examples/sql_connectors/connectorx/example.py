"""SQL column inference against connectorx.

`connectorx` is a high-performance SQL-to-DataFrame library used in performance-
sensitive pipelines, supporting many backends (Postgres, MySQL, SQL Server, and others)
through one API. Its `read_sql` takes the connection URI FIRST and the SQL text
SECOND — the reverse of `pd.read_sql(sql, conn)` — so typedframes special-cases its
argument order rather than reusing the pandas-convention extraction path.

Run: `typedframes check examples/sql_connectors/connectorx/`
Run for real: `docker compose up -d && uv run python example.py; docker compose down`

NOT independently verified end-to-end (no docker available in the environment this was
written in) -- the docker-compose/Postgres setup should work, but hasn't actually been
run against a live container.
"""

import connectorx
import connectorx as cx


def load_via_connectorx() -> None:
    """The idiomatic connectorx call: read_sql(conn_uri, query)."""
    df = cx.read_sql(
        "postgresql://user:pw@localhost:5433/mydb",
        "SELECT order_id, customer_id, amount FROM orders",
    )

    print(df["order_id"])  # OK
    print(df["amount"])  # OK
    # print(df["status"])  -- unknown-column: not in {order_id, customer_id, amount}


def load_via_module_alias() -> None:
    """Also recognized under the un-aliased `connectorx.read_sql(...)` spelling."""
    df = connectorx.read_sql(
        "postgresql://user:pw@localhost:5433/mydb",
        "SELECT product_id, price FROM products",
    )

    print(df["price"])  # OK
    # print(df["stock"])  -- unknown-column: not in {product_id, price}


def build_select_all_query(table: str) -> str:
    """A query built at runtime -- not a literal, so the checker can't resolve it."""
    return f"SELECT * FROM {table}"


def load_with_dynamic_query(table: str) -> None:
    """A query assembled by a helper function — deliberately unresolved."""
    query = build_select_all_query(table)
    df = cx.read_sql("postgresql://user:pw@localhost:5433/mydb", query)  # untracked-dataframe: not a literal
    print(df)


def load_with_unknown_column() -> None:
    """A real bug: 'status' is not in the inferred column set.

    Left out of __main__ below -- this is a static-analysis fixture, not meant to be
    executed. Run `typedframes check .` to see it caught as unknown-column.
    """
    df = cx.read_sql(
        "postgresql://user:pw@localhost:5433/mydb",
        "SELECT order_id, customer_id, amount FROM orders",
    )
    print(df["status"])  # unknown-column: not in {order_id, customer_id, amount}


if __name__ == "__main__":
    load_via_connectorx()
    load_via_module_alias()
    load_with_dynamic_query("products")
    # load_with_unknown_column() intentionally not run -- see its docstring.

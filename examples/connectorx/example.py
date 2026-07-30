"""SQL column inference against connectorx.

`connectorx` is a high-performance SQL-to-DataFrame library used in performance-
sensitive pipelines, supporting many backends (Postgres, MySQL, SQL Server, and others)
through one API. Its `read_sql` takes the connection URI FIRST and the SQL text
SECOND — the reverse of `pd.read_sql(sql, conn)` — so typedframes special-cases its
argument order rather than reusing the pandas-convention extraction path.

Run: `typedframes check examples/connectorx/`
"""

import connectorx as cx


def load_via_connectorx() -> None:
    """The idiomatic connectorx call: read_sql(conn_uri, query)."""
    df = cx.read_sql(
        "postgresql://user:pw@localhost:5432/mydb",
        "SELECT order_id, customer_id, amount FROM orders",
    )

    print(df["order_id"])  # OK
    print(df["amount"])  # OK
    # print(df["status"])  -- unknown-column: not in {order_id, customer_id, amount}


def load_via_module_alias() -> None:
    """Also recognized under the un-aliased `connectorx.read_sql(...)` spelling."""
    df = connectorx.read_sql(
        "postgresql://user:pw@localhost:5432/mydb",
        "SELECT product_id, price FROM products",
    )

    print(df["price"])  # OK
    # print(df["stock"])  -- unknown-column: not in {product_id, price}


def load_with_dynamic_query(table: str) -> None:
    """A query assembled by a helper function — deliberately unresolved."""
    query = build_select_all_query(table)
    df = cx.read_sql("postgresql://user:pw@localhost:5432/mydb", query)  # untracked-dataframe: not a literal
    print(df)

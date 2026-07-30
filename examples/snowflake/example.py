"""SQL column inference against Snowflake.

Snowflake genuinely upper-cases unquoted identifiers: `SELECT order_id FROM orders`
returns a column literally named `ORDER_ID`, not `order_id` (verified against
Snowflake's own identifier documentation — quoted identifiers like `"order_id"` are the
escape hatch that preserves case). With `sql_dialect = "snowflake"` set in this
directory's `pyproject.toml`, typedframes folds identifier case the same way at lint
time, so writing `df["order_id"]` against a Snowflake result is flagged as a real bug —
not silently accepted the way it would be under a naive case-insensitive design.

Run: `typedframes check examples/snowflake/`
"""

import os
from pathlib import Path

import pandas as pd


def load_via_cursor_literal() -> None:
    """snowflake-connector-python's native cursor pattern, with an inline query."""
    conn = connect(
        account="my_account",
        user="user",
        password=os.environ["SNOWFLAKE_PASSWORD"],
        warehouse="WH",
    )
    cursor = conn.cursor()
    cursor.execute("SELECT order_id, amount FROM orders")
    df = cursor.fetch_pandas_all()

    print(df["ORDER_ID"])  # OK -- Snowflake's real (uppercased) column name
    # print(df["order_id"])  -- unknown-column: the query text's own spelling,
    #   but not what Snowflake actually returns (did you mean 'ORDER_ID'?)


def load_via_sqlalchemy_engine() -> None:
    """The alternative snowflake-sqlalchemy + pd.read_sql path — same dialect folding."""
    engine = create_engine(os.environ["SNOWFLAKE_SQLALCHEMY_URL"])
    df = pd.read_sql("SELECT customer_id, region FROM customers", engine)

    print(df["CUSTOMER_ID"])  # OK
    # print(df["customer_id"])  -- unknown-column (did you mean 'CUSTOMER_ID'?)


def load_via_traced_variable() -> None:
    """A query kept in a single-assignment variable resolves like an inline literal.

    typedframes traces `query` back to its single assignment (see the checker's
    string_var_candidates) — reassigning it anywhere else in this file would make it
    unresolvable rather than risk guessing which assignment applied at the call site.
    """
    query = "SELECT order_id, status FROM orders WHERE status = 'completed'"
    conn = connect(
        account="my_account",
        user="user",
        password=os.environ["SNOWFLAKE_PASSWORD"],
        warehouse="WH",
    )
    cursor = conn.cursor()
    cursor.execute(query)
    df = cursor.fetch_pandas_all()

    print(df["STATUS"])  # OK
    # print(df["status"])  -- unknown-column (did you mean 'STATUS'?)


def load_via_query_file() -> None:
    """A query kept in orders.sql alongside this file, for readability/review.

    typedframes reads the file at lint time (project-root-relative only, capped in
    size, refuses anything that escapes the project root) to infer the same column set
    it would from an inline literal.
    """
    sql = (Path(__file__).parent / "orders.sql").read_text()
    conn = connect(
        account="my_account",
        user="user",
        password=os.environ["SNOWFLAKE_PASSWORD"],
        warehouse="WH",
    )
    cursor = conn.cursor()
    cursor.execute(sql)
    df = cursor.fetch_pandas_all()

    print(df["CUSTOMER_ID"])  # OK -- orders.sql selects order_id, customer_id, amount, status
    # print(df["customer_id"])  -- unknown-column (did you mean 'CUSTOMER_ID'?)


def load_with_dynamic_filter(customer_id: str) -> None:
    """A parameterized query — the PEP 249-recommended way to avoid SQL injection.

    typedframes doesn't (and shouldn't) try to resolve an f-string here: it has no
    taint analysis to distinguish a safe interpolation from a real vulnerability, so
    an unresolvable query just falls through to the untracked-dataframe hint rather
    than a noisy, unactionable injection warning.
    """
    conn = connect(
        account="my_account",
        user="user",
        password=os.environ["SNOWFLAKE_PASSWORD"],
        warehouse="WH",
    )
    cursor = conn.cursor()
    cursor.execute("SELECT order_id, amount FROM orders WHERE customer_id = %s", (customer_id,))
    df = cursor.fetch_pandas_all()  # untracked-dataframe: parameterized query, not a literal
    print(df)

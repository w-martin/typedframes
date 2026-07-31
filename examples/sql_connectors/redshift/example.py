"""SQL column inference against Redshift.

Redshift is Postgres-derived and folds unquoted identifiers to lower case (verified
against PostgreSQL's own lexical-structure documentation, which Redshift's unquoted-
identifier behavior matches) — the opposite direction from Snowflake. With
`sql_dialect = "redshift"` set in this directory's pyproject.toml, a MixedCase column
in the query text is inferred as its real, lower-cased runtime name.

Run: `typedframes check examples/sql_connectors/redshift/`
Run for real: `docker compose up -d && uv run python example.py; docker compose down`

NOT independently verified end-to-end (no docker available in the environment this was
written in). The psycopg2 path against real Postgres should be solid; `redshift_connector`
speaks a protocol forked from Postgres's, but whether its connect handshake actually
succeeds against a stock `postgres:16` server (vs. real Redshift, or nothing at all) is
unconfirmed -- if it rejects the connection, mock just that one call.
"""

import pandas as pd
import psycopg2
import redshift_connector


def load_via_cursor() -> None:
    """redshift_connector's native cursor pattern (PEP 249-style, like Snowflake's)."""
    conn = redshift_connector.connect(host="localhost", port=5435, database="dev", user="user", password="pw")
    cursor = conn.cursor()
    cursor.execute("SELECT OrderId, Revenue FROM Sales")
    df = cursor.fetch_pandas_all()

    print(df["orderid"])  # OK -- unquoted MixedCase folds to lower case at the engine
    print(df["revenue"])  # OK
    # print(df["OrderId"])  -- unknown-column: not the real (lower-cased) column name


def load_via_psycopg2() -> None:
    """Redshift is wire-compatible with Postgres, so psycopg2 + pd.read_sql also works."""
    conn = psycopg2.connect(host="localhost", port=5435, dbname="dev", user="user", password="pw")
    df = pd.read_sql("SELECT product_id, unit_price FROM products", conn)

    print(df["product_id"])  # OK -- already lower case in the query text
    # print(df["stock"])  -- unknown-column: not in {product_id, unit_price}


def load_with_percent_s_parameter(min_price: float) -> None:
    """A parameterized query using psycopg2's `%s` placeholder.

    Parameterized queries are the idiomatic (and PEP 249-recommended) way to avoid SQL
    injection — but `%s` isn't valid standalone SQL syntax on its own, so the query
    text doesn't parse even though it IS a plain string literal. Untracked, same as an
    f-string — just for a different reason.
    """
    conn = psycopg2.connect(host="localhost", port=5435, dbname="dev", user="user", password="pw")
    df = pd.read_sql(
        "SELECT product_id, unit_price FROM products WHERE unit_price > %s",
        conn,
        params=(min_price,),
    )  # untracked-dataframe: `%s` doesn't parse as SQL
    print(df)


def load_with_qmark_parameter(min_price: float) -> None:
    """A parameterized query using the `?` (qmark) placeholder.

    Unlike `%s`, `?` DOES parse as valid SQL syntax -- that's about typedframes' own SQL
    grammar accepting `?` as a placeholder token, so this one resolves normally despite
    also being parameterized. It is NOT a claim that psycopg2 itself supports qmark-style
    binding at runtime -- it doesn't (only `%s`/`%(name)s`), so this function is left out
    of the `__main__` block below; it would raise a real `psycopg2.errors.SyntaxError`
    against an actual server, which is a runtime-driver detail orthogonal to what this
    function is illustrating about the checker's parser.
    """
    conn = psycopg2.connect(host="localhost", port=5435, dbname="dev", user="user", password="pw")
    df = pd.read_sql(
        "SELECT product_id, unit_price FROM products WHERE unit_price > ?",
        conn,
        params=(min_price,),
    )
    print(df["product_id"])  # OK
    # print(df["stock"])  -- unknown-column: not in {product_id, unit_price}


def load_with_unknown_column() -> None:
    """A real bug: 'OrderId' is the query text's own spelling, not what Redshift
    actually returns.

    Left out of __main__ below -- this is a static-analysis fixture, not meant to be
    executed. Run `typedframes check .` to see it caught as unknown-column.
    """
    conn = redshift_connector.connect(host="localhost", port=5435, database="dev", user="user", password="pw")
    cursor = conn.cursor()
    cursor.execute("SELECT OrderId, Revenue FROM Sales")
    df = cursor.fetch_pandas_all()
    print(df["OrderId"])  # unknown-column: not the real (lower-cased) column name


if __name__ == "__main__":
    load_via_cursor()
    load_via_psycopg2()
    load_with_percent_s_parameter(min_price=0.0)
    # load_with_unknown_column() intentionally not run -- see its docstring.

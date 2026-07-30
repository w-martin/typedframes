"""SQL column inference against Redshift.

Redshift is Postgres-derived and folds unquoted identifiers to lower case (verified
against PostgreSQL's own lexical-structure documentation, which Redshift's unquoted-
identifier behavior matches) — the opposite direction from Snowflake. With
`sql_dialect = "redshift"` set in this directory's pyproject.toml, a MixedCase column
in the query text is inferred as its real, lower-cased runtime name.

Run: `typedframes check examples/redshift/`
"""

import pandas as pd


def load_via_cursor() -> None:
    """redshift_connector's native cursor pattern (PEP 249-style, like Snowflake's)."""
    conn = redshift_connector.connect(host="cluster.region.redshift.amazonaws.com", database="dev", user="awsuser")
    cursor = conn.cursor()
    cursor.execute("SELECT OrderId, Revenue FROM Sales")
    df = cursor.fetch_pandas_all()

    print(df["orderid"])  # OK -- unquoted MixedCase folds to lower case at the engine
    print(df["revenue"])  # OK
    # print(df["OrderId"])  -- unknown-column: not the real (lower-cased) column name


def load_via_psycopg2() -> None:
    """Redshift is wire-compatible with Postgres, so psycopg2 + pd.read_sql also works."""
    conn = psycopg2.connect(host="cluster.region.redshift.amazonaws.com", dbname="dev")
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
    conn = psycopg2.connect(host="cluster.region.redshift.amazonaws.com", dbname="dev")
    df = pd.read_sql(
        "SELECT product_id, unit_price FROM products WHERE unit_price > %s",
        conn,
        params=(min_price,),
    )  # untracked-dataframe: `%s` doesn't parse as SQL
    print(df)


def load_with_qmark_parameter(min_price: float) -> None:
    """A parameterized query using the `?` (qmark) placeholder.

    Unlike `%s`, `?` DOES parse as valid SQL syntax, so this one resolves normally
    despite also being parameterized.
    """
    conn = psycopg2.connect(host="cluster.region.redshift.amazonaws.com", dbname="dev")
    df = pd.read_sql(
        "SELECT product_id, unit_price FROM products WHERE unit_price > ?",
        conn,
        params=(min_price,),
    )
    print(df["product_id"])  # OK
    # print(df["stock"])  -- unknown-column: not in {product_id, unit_price}

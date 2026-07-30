"""SQL column inference against BigQuery.

BigQuery preserves the source case of unquoted identifiers (it only treats names
differing solely by case as duplicates/collisions) — `sql_dialect = "bigquery"` in this
directory's pyproject.toml is set for documentation/consistency, but doesn't change
column casing the way it does for Snowflake or Postgres/Redshift.

Run: `typedframes check examples/bigquery/`
"""

from pathlib import Path

import pandas as pd


def load_via_client_query() -> None:
    """The idiomatic google-cloud-bigquery pattern: client.query(sql).to_dataframe()."""
    client = bigquery.Client(project="my-project")
    df = client.query("SELECT user_id, total_spent, signup_date FROM `my-project.analytics.customers`").to_dataframe()

    print(df["user_id"])  # OK
    print(df["total_spent"])  # OK
    # print(df["email"])  -- unknown-column: 'email' not in {user_id, total_spent, signup_date}


def load_via_pandas_gbq() -> None:
    """The older pandas-gbq path, still common in notebook/pandas-first workflows."""
    df = pd.read_gbq(
        "SELECT user_id, region FROM `my-project.analytics.customers`",
        project_id="my-project",
    )

    print(df["region"])  # OK
    # print(df["total_spent"])  -- unknown-column: not in {user_id, region}


def load_via_query_file() -> None:
    """A query kept in customers.sql alongside this file."""
    sql = (Path(__file__).parent / "customers.sql").read_text()
    client = bigquery.Client(project="my-project")
    df = client.query(sql).to_dataframe()

    print(df["signup_date"])  # OK -- customers.sql selects user_id, total_spent, signup_date
    # print(df["last_login"])  -- unknown-column


def load_with_query_builder(dataset: str) -> None:
    """A query assembled by a helper function — deliberately not resolved.

    Only a plain string literal (or a single-assignment variable/`.sql` file — see the
    Snowflake example) is traced; any other expression, including a query-builder call,
    falls through to untracked-dataframe rather than being guessed at.
    """
    client = bigquery.Client(project="my-project")
    query = build_customers_query(dataset)
    df = client.query(query).to_dataframe()  # untracked-dataframe: not a literal
    print(df)

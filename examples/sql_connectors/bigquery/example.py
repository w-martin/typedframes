"""SQL column inference against BigQuery.

BigQuery preserves the source case of unquoted identifiers (it only treats names
differing solely by case as duplicates/collisions) — `sql_dialect = "bigquery"` in this
directory's pyproject.toml is set for documentation/consistency, but doesn't change
column casing the way it does for Snowflake or Postgres/Redshift.

Run: `typedframes check examples/sql_connectors/bigquery/`
Run for real: `docker compose up -d && uv run python example.py; docker compose down`

NOT independently verified end-to-end (no docker available in the environment this was
written in) -- the docker-compose/bigquery-emulator setup should work, but hasn't
actually been run against a live container.
"""

from pathlib import Path

import pandas as pd
from google.api_core.client_options import ClientOptions
from google.auth.credentials import AnonymousCredentials
from google.cloud import bigquery

EMULATOR_HOST = "http://localhost:9050"


def _emulator_client() -> bigquery.Client:
    """A client pointed at the local bigquery-emulator instead of real GCP."""
    return bigquery.Client(
        project="my-project",
        credentials=AnonymousCredentials(),
        client_options=ClientOptions(api_endpoint=EMULATOR_HOST),
    )


def load_via_client_query() -> None:
    """The idiomatic google-cloud-bigquery pattern: client.query(sql).to_dataframe()."""
    client = _emulator_client()
    df = client.query("SELECT user_id, total_spent, signup_date FROM `my-project.analytics.customers`").to_dataframe()

    print(df["user_id"])  # OK
    print(df["total_spent"])  # OK


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
    client = _emulator_client()
    df = client.query(sql).to_dataframe()

    print(df["signup_date"])  # OK -- customers.sql selects user_id, total_spent, signup_date
    # print(df["last_login"])  -- unknown-column


def load_with_query_builder(dataset: str) -> None:
    """A query assembled by a helper function — deliberately not resolved.

    Only a plain string literal (or a single-assignment variable/`.sql` file — see the
    Snowflake example) is traced; any other expression, including a query-builder call,
    falls through to untracked-dataframe rather than being guessed at.
    """
    client = _emulator_client()
    query = build_customers_query(dataset)
    df = client.query(query).to_dataframe()  # untracked-dataframe: not a literal
    print(df)


def build_customers_query(dataset: str) -> str:
    """A query built at runtime -- not a literal, so the checker can't resolve it."""
    return f"SELECT user_id, total_spent, signup_date FROM `my-project.{dataset}.customers`"


def load_with_unknown_column() -> None:
    """A real bug: 'email' is not in the inferred column set.

    Left out of __main__ below -- this is a static-analysis fixture, not meant to be
    executed. Run `typedframes check .` to see it caught as unknown-column.
    """
    client = _emulator_client()
    df = client.query("SELECT user_id, total_spent, signup_date FROM `my-project.analytics.customers`").to_dataframe()
    print(df["email"])  # unknown-column: not in {user_id, total_spent, signup_date}


if __name__ == "__main__":
    load_via_client_query()
    load_via_query_file()
    load_with_query_builder("analytics")
    # load_with_unknown_column() intentionally not run -- see its docstring.
    # load_via_pandas_gbq() is intentionally not run here: pandas-gbq builds its own
    # bigquery.Client internally with no clean way to redirect it at a local emulator
    # (no client_options/endpoint override), so it can only be smoke-tested against
    # real GCP, not this self-contained setup.

"""A tiny in-process stand-in for Snowflake -- no OSS/local Snowflake emulator exists
anywhere, unlike most of the other connectors in this repo.

Reproduces the one behavior this example is actually about: Snowflake genuinely
upper-cases unquoted identifiers. This is not a SQL engine -- it parses just enough of
a `SELECT ... FROM ...` list to fabricate a small, correctly-shaped, correctly-cased
result, which is enough for the demo functions in example.py to run for real and
produce genuine (fake-data) pandas DataFrames rather than needing a real Snowflake
account.

`connect()` stands in for `snowflake.connector.connect`; `create_engine()` stands in
for SQLAlchemy's, returning a plain DB-API2-shaped object that pandas' `read_sql`
handles via its generic (non-SQLAlchemy) fallback path.
"""

import re
from typing import Any

import pandas as pd

_SELECT_RE = re.compile(r"select\s+(.*?)\s+from\s+", re.IGNORECASE | re.DOTALL)


def _parse_select_columns(sql: str) -> list[str]:
    match = _SELECT_RE.search(sql)
    if not match:
        raise ValueError(f"fake_snowflake can't parse this as a SELECT: {sql!r}")
    return [c.strip().upper() for c in match.group(1).split(",")]


def _fake_value(column: str, row: int) -> Any:
    if column == "AMOUNT":
        return 150.0 + row * 10
    if column == "STATUS":
        return "completed"
    return f"{column.lower()}_{row}"


class _FakeCursor:
    def __init__(self) -> None:
        self._columns: list[str] = []

    def execute(self, sql: str, params: Any = None) -> "_FakeCursor":
        self._columns = _parse_select_columns(sql)
        return self

    @property
    def description(self) -> list[tuple]:
        # DB-API2 7-tuples: (name, type_code, display_size, internal_size, precision, scale, null_ok)
        return [(col, None, None, None, None, None, None) for col in self._columns]

    def fetchall(self) -> list[tuple]:
        return [tuple(_fake_value(col, row) for col in self._columns) for row in range(2)]

    def fetch_pandas_all(self) -> pd.DataFrame:
        return pd.DataFrame({col: [_fake_value(col, row) for row in range(2)] for col in self._columns})

    def close(self) -> None:
        pass


class _FakeConnection:
    def cursor(self) -> _FakeCursor:
        return _FakeCursor()


def connect(**kwargs: Any) -> _FakeConnection:
    return _FakeConnection()


def create_engine(url: str) -> _FakeConnection:
    return _FakeConnection()

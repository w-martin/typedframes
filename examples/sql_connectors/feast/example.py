"""SQL/feature column inference for the Feast feature store.

Feast's `get_historical_features`/`get_online_features` results always contain the
requested feature columns PLUS entity_df's own join keys and timestamp column — which
this checker can't resolve in general (entity_df is frequently a raw SQL string or an
inline DataFrame literal, and the timestamp column's name isn't fixed). Registering
only the features= names as an ordinary exact-match schema would make entity_df's own
columns a false unknown-column — so Feast results are registered as an *open* schema
instead: the known feature columns are recorded, but membership checks never fail for
a column this checker simply doesn't know about.

Unlike every other connector in this repo, there is no live unknown-column error case
here -- open schemas never fail a membership check by design, so no column access on a
Feast result is ever flagged, even a genuinely wrong one. `load_with_unresolvable_features`
is the untracked-dataframe (info) case; `load_with_full_feature_names_renamed_access`
demonstrates the false-negative tradeoff that makes an error case impossible here: a
real runtime KeyError that the checker stays silent on.

Run: `typedframes check examples/sql_connectors/feast/`
Run for real: `cd examples/sql_connectors/feast && uv run python example.py`
"""

import os
from datetime import datetime, timedelta
from pathlib import Path
from typing import Any

import pandas as pd
from feast import FeatureStore


def load_via_chained_form(store: FeatureStore, entity_df: pd.DataFrame) -> None:
    """The common one-liner: get_historical_features(...).to_df()."""
    df = store.get_historical_features(
        entity_df=entity_df,
        features=["driver_stats:conv_rate", "driver_stats:acc_rate"],
    ).to_df()

    print(df["conv_rate"])  # OK -- resolved from features=
    print(df["acc_rate"])  # OK
    # Entity join keys from entity_df (e.g. driver_id) are NOT flagged either, even
    # though they're not in the known {conv_rate, acc_rate} set -- this is an open
    # schema, precisely because entity_df's real columns aren't enumerable here:
    print(df["driver_id"])  # OK -- open schema: never a false unknown-column


def load_via_split_form(store: FeatureStore, entity_df: pd.DataFrame) -> None:
    """The split form: the RetrievalJob is held in an intermediate variable.

    Before `.to_df()` is called, `job` is NOT tracked as a DataFrame at all -- it's a
    RetrievalJob, and subscripting it would be a real error the checker doesn't (and
    shouldn't) suppress.
    """
    job = store.get_historical_features(
        entity_df=entity_df,
        features=["driver_stats:conv_rate"],
    )
    df = job.to_df()

    print(df["conv_rate"])  # OK
    print(df["driver_id"])  # OK -- open schema, same as the chained form


def load_with_full_feature_names(store: FeatureStore, entity_df: pd.DataFrame) -> None:
    """full_feature_names=True renames columns to view__feature (double underscore)."""
    df = store.get_historical_features(
        entity_df=entity_df,
        features=["driver_stats:conv_rate", "driver_stats:acc_rate"],
        full_feature_names=True,
    ).to_df()

    print(df["driver_stats__conv_rate"])  # OK
    print(df["driver_stats__acc_rate"])  # OK
    # print(df["conv_rate"]) would also be OK here, not unknown-column -- see
    # load_with_full_feature_names_renamed_access() below for why.


def load_with_full_feature_names_renamed_access(store: FeatureStore, entity_df: pd.DataFrame) -> None:
    """A genuine runtime bug the checker deliberately does NOT catch.

    `full_feature_names=True` renames `conv_rate` to `driver_stats__conv_rate`, so
    `df["conv_rate"]` is a real `KeyError` at runtime -- but typedframes reports no
    error here at all, because Feast results are an *open* schema (see the module
    docstring): membership checks never fail, for the same reason `df["driver_id"]`
    isn't flagged elsewhere in this file either. This is the false-negative price of
    avoiding false positives on entity_df's own (unenumerable) columns. There is no
    way to produce a genuine unknown-column error against a Feast result at all.

    Left out of __main__ below -- it really does raise KeyError if run.
    """
    df = store.get_historical_features(
        entity_df=entity_df,
        features=["driver_stats:conv_rate"],
        full_feature_names=True,
    ).to_df()
    print(df["conv_rate"])  # real KeyError at runtime; no static warning either (open schema)


def load_online_features(store: FeatureStore) -> None:
    """get_online_features(...).to_df() — the serving-time equivalent."""
    df = store.get_online_features(
        features=["driver_stats:conv_rate", "driver_stats:avg_daily_trips"],
        entity_rows=[{"driver_id": 1001}],
    ).to_df()

    print(df["conv_rate"])  # OK
    print(df["avg_daily_trips"])  # OK
    print(df["driver_id"])  # OK -- open schema: entity_rows' own keys are never flagged


def load_with_unresolvable_features(store: FeatureStore, entity_df: pd.DataFrame, feature_names: list[str]) -> None:
    """A dynamically-built features list — deliberately unresolved.

    Unlike the open-schema cases above (which still know SOME columns), this one
    knows NOTHING about the result, so it falls through to the ordinary
    untracked-dataframe hint instead.
    """
    df = store.get_historical_features(
        entity_df=entity_df, features=feature_names
    ).to_df()  # untracked-dataframe: features= isn't a literal list
    print(df)


def to_df_on_unrelated_object_is_left_alone(some_other_result: Any) -> None:
    """`.to_df()` on something unrelated to a recognized Feast retrieval call.

    Not touched at all -- no false untracked-dataframe warning, no dataframes_total
    bump, since plenty of unrelated code has a `.to_df()` method for something else.
    """
    df = some_other_result.to_df()
    print(df)


if __name__ == "__main__":
    from feast_repo_setup import build_store

    os.chdir(Path(__file__).parent)
    store = build_store()

    entity_df = pd.DataFrame(
        {
            "driver_id": [1001, 1002],
            "event_timestamp": [datetime.now() - timedelta(minutes=30)] * 2,
        }
    )

    load_via_chained_form(store, entity_df)
    load_via_split_form(store, entity_df)
    load_with_full_feature_names(store, entity_df)
    load_online_features(store)
    load_with_unresolvable_features(store, entity_df, ["driver_stats:conv_rate"])
    # load_with_full_feature_names_renamed_access() intentionally not run -- it really
    # does raise KeyError; see its docstring for why the checker doesn't catch it.

    class _FakeRetrievalResult:
        def to_df(self) -> pd.DataFrame:
            return pd.DataFrame({"anything": [1, 2, 3]})

    to_df_on_unrelated_object_is_left_alone(_FakeRetrievalResult())

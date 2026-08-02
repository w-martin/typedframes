"""SQL/feature column inference for the Feast feature store.

Feast's `get_historical_features`/`get_online_features` results always contain the
requested feature columns PLUS entity_df's own join keys and timestamp column — which
this checker can't resolve in general (entity_df is frequently a raw SQL string or an
inline DataFrame literal, and the timestamp column's name isn't fixed). Registering
only the features= names as an ordinary exact-match schema would make entity_df's own
columns a false unknown-column — so Feast results are registered as an *open* schema
instead: the known feature columns are recorded, but membership checks never fail for
a column this checker simply doesn't know about.

Directly *on a Feast result's own open schema*, there is no live unknown-column error
case -- membership checks never fail by design, so no column access on a Feast result
is ever flagged, even a genuinely wrong one. `load_with_full_feature_names_renamed_access`
demonstrates the false-negative tradeoff that makes an open-schema error impossible: a
real runtime KeyError that the checker stays silent on. `load_feature_set`'s first call
site in `__main__` is the untracked-dataframe (info) case -- see below for why that's a
property of the CALL SITE, not the function.

There IS a different, real error case, though: `load_feature_by_name` below takes its
`features=` list as a parameter rather than a literal. typedframes traces a *literal*
argument from each CALL SITE back through that parameter -- so two callers of the exact
same function, passing two different literals, are validated completely independently
against the exact same internal `print(df["conv_rate"])` line, with the diagnostic
attributed to whichever call site actually got it wrong. The literal doesn't have to be
written out at the call site itself, either: `_forward_to_conv_rate_helper()` is a
zero-arg function that just forwards to *another* zero-arg function that returns the
literal, and typedframes follows that whole chain (as many hops as needed, with
recursion protection against cycles) to resolve it. Nor does the callee itself have to
return a literal at all: `_get_feature_names_dynamically("driver_stats")` takes a real
argument and builds its return value with an f-string -- typedframes substitutes the
literal `"driver_stats"` for that function's own parameter and evaluates the f-string
with it, arriving at the same resolved feature list as if the caller had written it out
directly. This only goes as far as a literal can actually be traced, though: a prefix
read from an environment variable at runtime has nothing for the tracer to substitute
-- and unlike the callee's own generic fallback, that untracked-dataframe diagnostic is
now reported right at the call site that couldn't provide one, not inside whichever
function it happens to call. See `__main__` below.

Run: `typedframes check examples/sql_connectors/feast/`
Run for real: `cd examples/sql_connectors/feast && uv run python example.py`
"""

import os
from datetime import datetime, timedelta
from pathlib import Path
from typing import TYPE_CHECKING, Any

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


def load_feature_set(store: FeatureStore, entity_df: pd.DataFrame, feature_names: list[str]) -> None:
    """A parameter-governed features= call, same shape as load_feature_by_name below.

    Its own body makes no column access to validate (just `print(df)`) -- the function
    itself is exactly as resolvable as load_feature_by_name is, in the abstract.
    Whether a given call site ends up OK or gets its own untracked-dataframe info note
    depends entirely on what THAT call site passes, not on anything about this function
    -- see __main__ for a call site whose argument can't be traced to a literal.
    """
    df = store.get_historical_features(entity_df=entity_df, features=feature_names).to_df()
    print(df)


def _get_feature_names_dynamically(prefix: str) -> list[str]:
    """Builds its return value with an f-string rather than returning a literal list.

    typedframes traces a *literal* argument through this too (see __main__):
    `_get_feature_names_dynamically("driver_stats")` is resolved by substituting
    "driver_stats" for `prefix` and evaluating the f-string with it, exactly as if the
    caller had written out `["driver_stats:conv_rate"]` directly. Passed a
    non-literal prefix instead (see `load_feature_set`'s first caller in __main__),
    it's unresolvable, same as ever.
    """
    return [f"{prefix}:conv_rate"]


def load_feature_by_name(store: FeatureStore, entity_df: pd.DataFrame, feature_names: list[str]) -> None:
    """A parameter-governed features= call, same shape as load_feature_set above.

    The difference here is that this function's own body makes a real column access
    (`print(df["conv_rate"])`) to validate against each call site's resolved features.
    typedframes traces a *literal* argument from each call site back through this
    function's own `feature_names` parameter, independently per caller: passing
    `["driver_stats:conv_rate"]` makes that access valid; passing anything else makes
    it a real unknown-column error — reported at THAT call site, not here, since this
    line is one single, caller-independent location that different callers can validly
    disagree about. See __main__.
    """
    df = store.get_historical_features(entity_df=entity_df, features=feature_names).to_df()
    print(df["conv_rate"])


def _conv_rate_feature_list() -> list[str]:
    """A zero-arg helper whose entire body is a literal return -- resolved directly."""
    return ["driver_stats:conv_rate"]


def _forward_to_conv_rate_helper() -> list[str]:
    """Forwards to _conv_rate_feature_list() with no arguments of its own.

    typedframes follows a `return <zero-arg call>()` chain as many hops as needed
    (with cycle protection) to reach a literal — so a caller passing THIS function's
    result resolves exactly as if it had passed the literal directly, even though
    neither this function nor its own caller ever writes the literal out. See the
    third call to load_feature_by_name in __main__.
    """
    return _conv_rate_feature_list()


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
    # The prefix comes from a runtime environment variable -- no literal anywhere in
    # this call chain, so typedframes can't trace it to a literal and reports an
    # untracked-dataframe info note right at THIS call site (not inside
    # load_feature_set, which is exactly as resolvable as load_feature_by_name):
    runtime_prefix = os.environ.get("FEAST_VIEW_PREFIX", "driver_stats")
    load_feature_set(store, entity_df, _get_feature_names_dynamically(runtime_prefix))  # untracked-dataframe
    # load_with_full_feature_names_renamed_access() intentionally not run -- it really
    # does raise KeyError; see its docstring for why the checker doesn't catch it.

    load_feature_by_name(store, entity_df, ["driver_stats:conv_rate"])  # OK -- resolved cleanly at THIS call site
    load_feature_by_name(store, entity_df, _forward_to_conv_rate_helper())  # OK -- resolved through a 2-hop chain
    load_feature_by_name(
        store, entity_df, _get_feature_names_dynamically("driver_stats")
    )  # OK -- argument substitution

    if TYPE_CHECKING:
        # Never executes (TYPE_CHECKING is always False at runtime) -- this call site
        # passes the WRONG literal for load_feature_by_name's internal
        # print(df["conv_rate"]), and typedframes catches it right here, without
        # touching the clean call site above at all:
        load_feature_by_name(store, entity_df, ["driver_stats:acc_rate"])  # unknown-column

    class _FakeRetrievalResult:
        def to_df(self) -> pd.DataFrame:
            return pd.DataFrame({"anything": [1, 2, 3]})

    to_df_on_unrelated_object_is_left_alone(_FakeRetrievalResult())

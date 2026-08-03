"""One-time local Feast repo bootstrap: apply definitions, materialize the online store.

Kept separate from example.py so the checker demo file only contains the patterns
typedframes actually recognizes -- this is just test-fixture plumbing.
"""

import sys
from datetime import datetime, timedelta
from pathlib import Path

from feast import FeatureStore

sys.path.insert(0, str(Path(__file__).parent / "feature_repo"))


def build_store() -> FeatureStore:
    from definitions import driver, driver_stats_fv

    store = FeatureStore(repo_path="feature_repo")
    store.apply([driver, driver_stats_fv])
    store.materialize_incremental(end_date=datetime.now() + timedelta(minutes=1))
    return store

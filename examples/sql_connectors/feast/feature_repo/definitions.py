"""Feast feature definitions for the runnable version of this example.

A minimal driver_stats feature view, backed by a local parquet file, registered and
materialized programmatically from example.py's __main__ block rather than via the
`feast apply` CLI -- keeps the whole example runnable with one `uv run python
example.py`.
"""

from datetime import timedelta

from feast import Entity, FeatureView, Field, FileSource
from feast.types import Float32, Int64

driver = Entity(name="driver", join_keys=["driver_id"])

driver_stats_source = FileSource(
    path="data/driver_stats.parquet",
    timestamp_field="event_timestamp",
    created_timestamp_column="created_timestamp",
)

driver_stats_fv = FeatureView(
    name="driver_stats",
    entities=[driver],
    ttl=timedelta(days=3650),
    schema=[
        Field(name="conv_rate", dtype=Float32),
        Field(name="acc_rate", dtype=Float32),
        Field(name="avg_daily_trips", dtype=Int64),
    ],
    source=driver_stats_source,
)

"""Stand-in for `databricks.connect.DatabricksSession`.

There is no OSS/local Databricks emulator, but databricks-connect is itself built on
Apache Spark's own (fully open-source) Spark Connect protocol -- so this wraps a real
local Spark Connect server (started by docker-compose from the official `apache/spark`
image) behind the same `DatabricksSession.builder.host(...).create()` call shape real
Databricks code uses. Only the class name/shape is fake; the SQL execution underneath
is real Spark.
"""

from pyspark.sql import SparkSession

SPARK_CONNECT_URL = "sc://localhost:15002"


class _DatabricksSessionBuilder:
    def host(self, _workspace_url: str) -> "_DatabricksSessionBuilder":
        # Real databricks-connect uses the host to route to a Databricks workspace;
        # here we always connect to the local Spark Connect server regardless.
        return self

    def create(self) -> SparkSession:
        return SparkSession.builder.remote(SPARK_CONNECT_URL).getOrCreate()


class DatabricksSession:
    builder = _DatabricksSessionBuilder()

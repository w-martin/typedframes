# SQL / Data-Warehouse Connectors

SQL column inference — the column set is inferred from a query's `SELECT` list
instead of `usecols=`/`columns=`, including tracing the query back through a
single-assignment variable or a `.sql` file, and dialect-aware identifier case
folding (`sql_dialect` in `pyproject.toml`).

Each directory is a self-contained, runnable `uv` project targeting a real (or
realistically mocked/emulated) backend — no live cloud account needed. None of these
connector SDKs are dependencies of the core `typedframes` package; they live only in
each example's own workspace-excluded `pyproject.toml`.

Every directory explicitly illustrates all three things `typedframes check` can
report, mirroring `examples/features/multi_file_inference/`'s convention: real column
access that's OK, a live `untracked-dataframe` warning case (an unresolvable query — a
dynamic/f-string-built one, or a parameterized query the SQL grammar can't parse), and
a dedicated `load_with_unknown_column()` function with a genuine, uncommented bug —
left out of the `__main__` block (it would raise a real `KeyError` if actually run) so
`uv run python example.py` stays runnable while `typedframes check .` still catches it.
`feast/` is a partial exception: directly on a Feast result's own *open* schema, no
column access is ever flagged, even a genuinely wrong one — see its module docstring
for why. But it also demonstrates a different, real error case that only Feast's
example currently exercises: call-site argument tracing, where a function's
`features=` parameter is resolved independently per caller (see [docs/usage.md's
"Call-site argument tracing"](../../../docs/usage.md#call-site-argument-tracing-feast-features)).

| Directory | Backend | How it runs |
|---|---|---|
| [`duckdb/`](duckdb/) | Real, embedded | No setup — runs against local parquet fixtures |
| [`feast/`](feast/) | Real, Feast local mode | No setup — sqlite registry + local parquet fixtures |
| [`connectorx/`](connectorx/) | Real Postgres | `docker compose up -d` |
| [`sqlalchemy/`](sqlalchemy/) | Real Postgres | `docker compose up -d` |
| [`redshift/`](redshift/) | Real Postgres (Redshift wire-compatible) | `docker compose up -d` |
| [`bigquery/`](bigquery/) | Real, `bigquery-emulator` | `docker compose up -d` |
| [`athena/`](athena/) | Real, via `moto` | No setup — in-process AWS mock |
| [`pyspark/`](pyspark/) | Real, embedded Spark | `docker compose run --rm app` (JDK image) |
| [`databricks/`](databricks/) | Real local Spark Connect server behind a fake `DatabricksSession` shim | `docker compose up -d` |
| [`snowflake/`](snowflake/) | Fake cursor/connection (no OSS Snowflake emulator exists) | No setup — in-process stand-in |
| [`azure_synapse/`](azure_synapse/) | Real MS SQL Server | `docker compose up -d` (custom image w/ msodbcsql18) |

## duckdb/

Embedded — no external service. `orders.parquet`/`products.parquet` are fixtures
checked into the directory (generated once via `duckdb`'s own `COPY ... TO ...
(FORMAT parquet)`, matching the columns each demo function's query selects).

```shell
cd examples/sql_connectors/duckdb
uv sync
uv run python example.py
uv run typedframes check .
```

## feast/

Feast's own local mode: sqlite registry + sqlite online store + a file (parquet)
offline source, all under `feature_repo/`. `feast_repo_setup.py` applies the
`driver_stats` entity/feature-view definitions and materializes the online store —
kept out of `example.py` so that file only shows the patterns typedframes actually
recognizes. `load_feature_by_name` demonstrates call-site argument tracing: two call
sites in `__main__` pass different literal `features=` lists to the same function, and
are validated completely independently against that function's own `print(df[...])`
line — the bad one is guarded behind `if TYPE_CHECKING:` so it's visible to the checker
without ever actually running.

```shell
cd examples/sql_connectors/feast
uv sync
uv run python example.py
uv run typedframes check .
```

## connectorx/

Real Postgres via `docker-compose.yml` (host port 5433, seeded from `seed.sql` on
first start).

```shell
cd examples/sql_connectors/connectorx
docker compose up -d
uv sync
uv run python example.py
uv run typedframes check .
docker compose down
```

> **Not independently verified end-to-end** — this was written in an environment with
> no `docker` available. `uv sync` and `typedframes check` both pass; the docker-compose
> setup itself hasn't actually been run against a live container.

## sqlalchemy/

Real Postgres via `docker-compose.yml` (host port 5434). `_seed_database()` in
`example.py` drops/recreates the schema from the declarative models themselves
(`Base.metadata.create_all`) and inserts fixture rows — no separate seed.sql, so the
schema can never drift from what the models declare.

```shell
cd examples/sql_connectors/sqlalchemy
docker compose up -d
uv sync
uv run python example.py
uv run typedframes check .
docker compose down
```

> **Not independently verified end-to-end** — this was written in an environment with
> no `docker` available. `uv sync` and `typedframes check` both pass, and the ORM model
> wiring (foreign key, relationship, renamed column) was smoke-tested against an
> in-memory SQLite engine; the docker-compose/Postgres setup itself hasn't actually
> been run against a live container.

## redshift/

Real Postgres via `docker-compose.yml` (host port 5435, seeded from `seed.sql`) —
Postgres folds unquoted identifiers to lower case the same way Redshift does, so the
MixedCase-folding demo works unmodified. `load_with_qmark_parameter` is deliberately
left out of the `__main__` run — psycopg2 doesn't support `?`-style parameter binding
at runtime (only `%s`), so it's a checker-grammar illustration only, not something to
execute for real.

```shell
cd examples/sql_connectors/redshift
docker compose up -d
uv sync
uv run python example.py
uv run typedframes check .
docker compose down
```

> **Not independently verified end-to-end** — this was written in an environment with
> no `docker` available. `uv sync` and `typedframes check` both pass. The psycopg2 path
> should be solid against real Postgres; whether `redshift_connector`'s connect
> handshake actually succeeds against a stock `postgres:16` server (rather than real
> Redshift) is unconfirmed — mock that one call if it doesn't.

## bigquery/

Real `ghcr.io/goccy/bigquery-emulator` via `docker-compose.yml`, seeded from
`seed-data.yaml` (project `my-project`, dataset `analytics`, table `customers`).
`_emulator_client()` points `google.cloud.bigquery.Client` at the emulator's REST
endpoint with anonymous credentials instead of real GCP. `load_via_pandas_gbq` is
intentionally left out of the `__main__` run — `pandas-gbq` builds its own client
internally with no endpoint override, so it can only be exercised against real GCP.

```shell
cd examples/sql_connectors/bigquery
docker compose up -d
uv sync
uv run python example.py
uv run typedframes check .
docker compose down
```

> **Not independently verified end-to-end** — this was written in an environment with
> no `docker` available. `uv sync`, module import, and `typedframes check` all pass,
> and the emulator-pointed client constructs without error; the docker-compose +
> `bigquery-emulator` YAML seed format itself hasn't actually been run against a live
> container — double-check the seed schema against the emulator's own docs if it
> doesn't load.

## athena/

`@mock_aws`, in-process, no docker or AWS account. Moto's Athena mock only fakes the
control-plane API (query submission/status) and returns 0 rows by default, so
`moto_setup.py` uses moto's own "static API"
(`POST http://motoapi.amazonaws.com/moto-api/static/athena/query-results`) to queue a
FIFO list of fake result sets — one per query `example.py` runs, in order.

```shell
cd examples/sql_connectors/athena
uv sync
uv run python example.py
uv run typedframes check .
```

Fully verified end-to-end, including the moto result-queue mechanism.

## pyspark/

Embedded Spark, local mode — no external service, just a JVM. `Dockerfile` is
`eclipse-temurin:17-jre-jammy` + `uv sync`, since this sandbox has no local JDK.
`_seed_orders()` registers an in-memory `orders` temp view before the demo functions
run their queries against it.

```shell
cd examples/sql_connectors/pyspark
docker compose run --rm app
uv run typedframes check .   # works with or without a JVM -- purely static
```

> **Not independently verified end-to-end** — this was written in an environment with
> no JVM/docker available. `uv sync`, module import, and `typedframes check` all pass;
> the actual Spark session/query hasn't been run.

## databricks/

No OSS Databricks emulator exists, but `databricks-connect` is itself built on Apache
Spark's own open-source Spark Connect protocol. `databricks_session_shim.py` wraps a
real local Spark Connect server (docker-compose, official `apache/spark:3.5.3` image)
behind the same `DatabricksSession.builder.host(...).create()` shape real code uses —
only the class is fake, the SQL execution underneath is real Spark. `pyspark[connect]`
is pinned to exactly `3.5.3` client-side to match the server image, since Spark
Connect's protobuf wire protocol isn't guaranteed compatible across versions.
`load_via_sql_connector` (the `databricks-sql-connector`/Thrift path) has no stand-in
at all — real proprietary protocol, no real endpoint — so it's left un-run.

```shell
cd examples/sql_connectors/databricks
docker compose up -d
uv sync
uv run python example.py
uv run typedframes check .
docker compose down
```

> **Not independently verified end-to-end** — this was written in an environment with
> no docker/JVM available, and of everything in this repo carries the most residual
> uncertainty (Spark Connect server bring-up via `--packages`, which needs network
> access to resolve at container start). `uv sync`, module import, and
> `typedframes check` all pass; the actual Spark Connect session hasn't been run.

## snowflake/

No OSS/local Snowflake emulator exists anywhere, so `fake_snowflake.py` provides a
hand-rolled `connect()` (stands in for `snowflake.connector.connect`) and
`create_engine()` (stands in for SQLAlchemy's) that parse just enough of a `SELECT`
list to fabricate a small, correctly upper-cased result — the one behavior this example
is actually about. Not a SQL engine; clearly labeled as a stand-in in both the module
docstring and inline. `load_via_lowercasing_wrapper` demonstrates the common
internal-package pattern of case-folding a connector's result before returning it — see
[docs/usage.md's "Supported column-set transforms"](../../../docs/usage.md#supported-column-set-transforms).

```shell
cd examples/sql_connectors/snowflake
uv sync
uv run python example.py
uv run typedframes check .
```

Fully verified end-to-end, including the fake cursor/engine plumbing.

## azure_synapse/

Real `mcr.microsoft.com/mssql/server:2022-latest` via `docker-compose.yml`. `pyodbc`
needs the `msodbcsql18` ODBC driver, which isn't a pip package — `Dockerfile` installs
it from Microsoft's own apt repo (accepting their ODBC EULA at build time) alongside
the app. `_seed_database()` drops/recreates `dbo.Orders`. `load_via_fabric_spark_notebook`
is intentionally left out of the `__main__` run — it needs a Spark runtime, which this
directory doesn't provision (see `examples/sql_connectors/pyspark/` for that).

```shell
cd examples/sql_connectors/azure_synapse
docker compose up -d
docker compose run --rm app
uv run typedframes check .
docker compose down
```

> **Not independently verified end-to-end** — this was written in an environment with
> no `docker` available, and confirmed one concrete thing along the way: `pyodbc`
> cannot even be *imported* on a plain macOS host without the system `unixodbc`
> library (`dlopen ... libodbc.2.dylib`), which is exactly why the driver has to be
> installed inside the container rather than assumed present. `typedframes check`
> passes; the docker-compose/SQL Server + msodbcsql18 setup itself hasn't actually been
> run against a live container.

Each directory's own README section has exact run instructions. In every case:

```shell
typedframes check examples/sql_connectors/<name>/
```

works standalone with no environment at all — the checker is purely static (AST-based
pattern matching over the query text), so it never needs the actual connector
installed or a real backend running. The environments above are for *executing* the
example scripts end-to-end, which is a separate, stronger guarantee than the static
check passing.

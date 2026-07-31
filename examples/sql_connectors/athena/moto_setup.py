"""Moto plumbing for the runnable version of this example.

Moto's Athena mock fakes the control-plane API (query submission/status) but doesn't
execute SQL against real data -- `get_query_results` returns 0 rows by default. Moto
exposes a dedicated "static API" to pre-load a FIFO queue of fake result sets instead;
each `get_query_results` call for a not-yet-seen QueryExecutionId pops the next queued
result. Queue results in the same order example.py's functions run their queries.

Kept out of example.py so that file only shows the patterns typedframes recognizes.
"""

import boto3
import requests


def _column(name: str, type_: str) -> dict:
    return {
        "CatalogName": "hive",
        "SchemaName": "",
        "TableName": "",
        "Name": name,
        "Label": name,
        "Type": type_,
        "Precision": 0,
        "Scale": 0,
        "Nullable": "UNKNOWN",
        "CaseSensitive": True,
    }


def _row(*values: str) -> dict:
    return {"Data": [{"VarCharValue": v} for v in values]}


def setup_moto_athena() -> None:
    """Create the S3 staging bucket and queue the three queries' worth of fake results."""
    s3 = boto3.client("s3", region_name="us-east-1")
    s3.create_bucket(Bucket="my-bucket")

    expected_results = {
        "results": [
            {
                # load_via_pyathena(): SELECT order_id, customer_id, amount FROM orders
                "rows": [
                    _row("order_id", "customer_id", "amount"),
                    _row("1", "101", "150.00"),
                    _row("2", "102", "89.50"),
                ],
                "column_info": [
                    _column("order_id", "integer"),
                    _column("customer_id", "integer"),
                    _column("amount", "double"),
                ],
            },
            {
                # load_via_query_file(): orders.sql -> order_id, amount, status
                "rows": [
                    _row("order_id", "amount", "status"),
                    _row("1", "150.00", "completed"),
                    _row("2", "89.50", "completed"),
                ],
                "column_info": [
                    _column("order_id", "integer"),
                    _column("amount", "double"),
                    _column("status", "varchar"),
                ],
            },
            {
                # load_with_dynamic_table("orders"): SELECT * FROM orders
                "rows": [
                    _row("order_id", "customer_id", "amount", "status"),
                    _row("1", "101", "150.00", "completed"),
                    _row("2", "102", "89.50", "completed"),
                ],
                "column_info": [
                    _column("order_id", "integer"),
                    _column("customer_id", "integer"),
                    _column("amount", "double"),
                    _column("status", "varchar"),
                ],
            },
        ]
    }
    resp = requests.post(
        "http://motoapi.amazonaws.com/moto-api/static/athena/query-results",
        json=expected_results,
    )
    resp.raise_for_status()

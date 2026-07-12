"""Report builder whose parameter declares the columns it needs via a schema.

``build_customer_report``'s parameter is annotated with ``ReportSchema`` — the
schema that composes ``OrderSchema`` and ``CustomerSchema`` (see schemas.py).
That annotation *is* the function's contract: the checker validates every
column access in this file against it, and — because it's a schema, not just
an inferred set — that same column list becomes the authoritative requirement
checked at every call site, cross-file, the same way ``load_orders``'s return
type travels into pipeline.py.

See ``pipeline.py::report_missing_columns`` for what happens when a caller
passes a frame that doesn't actually satisfy ``ReportSchema``.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Annotated

from schemas import ReportSchema

if TYPE_CHECKING:
    import polars as pl


def build_customer_report(report: Annotated[pl.DataFrame, ReportSchema]) -> None:
    """Print every ReportSchema column — requires the full joined schema."""
    print(report["order_id"])
    print(report["customer_id"])
    print(report["amount"])
    print(report["status"])
    print(report["name"])
    print(report["region"])
    print(report["amount_vat"])

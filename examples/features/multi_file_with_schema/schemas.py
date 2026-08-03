"""Shared schema definitions.

``BaseSchema`` classes give the checker a named, stable column set that travels
with the DataFrame as it moves across module boundaries.  Functions that return
``Annotated[pd.DataFrame, OrderSchema]`` carry the full column set into every
call site — no ``usecols=`` required at the load point and no inference gap at
the consumer.

The descriptors (``OrderSchema.amount``, ``OrderSchema.customer_id``, …) also
give you IDE autocomplete and rename-safe column references via ``.s`` (string)
and ``.col`` (polars expression), but they are orthogonal to the checker — the
checker validates string literals like ``df["amount"]`` equally well.
"""

from __future__ import annotations

from typedframes import BaseSchema, Column


class OrderSchema(BaseSchema):
    """Schema for a raw order record."""

    order_id = Column(type=int)
    customer_id = Column(type=int)
    amount = Column(type=float)
    status = Column(type=str)


class CustomerSchema(BaseSchema):
    """Schema for a customer record."""

    customer_id = Column(type=int)
    name = Column(type=str)
    region = Column(type=str)


class ReportSchema(OrderSchema, CustomerSchema):
    """Schema for the joined reporting view.

    Composes OrderSchema and CustomerSchema via multiple inheritance — the
    checker inherits all columns from both parents automatically, so only
    the new column needs to be declared here.
    """

    amount_vat = Column(type=float)

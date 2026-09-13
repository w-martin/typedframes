"""Genuinely-executable companion to `example.py`.

`example.py` is a linter fixture: it deliberately includes calls that can never
succeed at runtime (`cudf.read_sql`, which cuDF does not export) so the checker has
something to leave untracked. That makes `example.py` itself unsuitable to just run.

This script exists to prove the *valid* operations `example.py` demonstrates --
`read_parquet(columns=...)`, `rename`, `drop`, `assign`, `merge` -- are real cuDF/pandas
API usage, not something invented to satisfy a linter. It generates its own small
synthetic fixtures on disk (no binary files checked into the repo) and runs the same
schema-shaped pipeline against them.

Requires an NVIDIA GPU and a working RAPIDS/cuDF install -- see docker-compose.yml.
"""

from pathlib import Path
from typing import Annotated

import cudf
import pandas as pd

from typedframes import BaseSchema, Column

FIXTURE_DIR = Path(__file__).parent / "_fixtures"


class OrderSchema(BaseSchema):
    """Schema for the orders table."""

    order_id = Column(type=int)
    customer_id = Column(type=int)
    amount = Column(type=float)


class CustomerSchema(BaseSchema):
    """Schema for the customers table."""

    customer_id = Column(type=int)
    region = Column(type=str)


def generate_fixtures() -> None:
    """Write small synthetic orders/customers/totals fixtures for this run."""
    FIXTURE_DIR.mkdir(exist_ok=True)

    orders = pd.DataFrame(
        {
            "order_id": [1, 2, 3, 4],
            "customer_id": [10, 10, 20, 30],
            "amount": [9.99, 19.99, 4.50, 100.00],
        }
    )
    orders.to_parquet(FIXTURE_DIR / "orders.parquet")

    customers = pd.DataFrame(
        {
            "customer_id": [10, 20, 30],
            "region": ["EMEA", "APAC", "AMER"],
        }
    )
    customers.to_parquet(FIXTURE_DIR / "customers.parquet")

    totals = pd.DataFrame(
        {
            "order_id": [1, 2, 3, 4],
            "total": [9.99, 19.99, 4.50, 100.00],
        }
    )
    totals.to_csv(FIXTURE_DIR / "totals.csv", index=False)


def load_orders() -> Annotated[cudf.DataFrame, OrderSchema]:
    """Read orders onto the GPU and assert OrderSchema."""
    return cudf.read_parquet(FIXTURE_DIR / "orders.parquet", columns=["order_id", "customer_id", "amount"])


def load_customers() -> Annotated[cudf.DataFrame, CustomerSchema]:
    """Read customers onto the GPU and assert CustomerSchema."""
    return cudf.read_parquet(FIXTURE_DIR / "customers.parquet", columns=["customer_id", "region"])


def main() -> None:
    """Run only the operations `example.py` shows as valid, against real data."""
    generate_fixtures()

    orders = load_orders()
    print("orders:")
    print(orders[["order_id", "amount"]])

    totals = cudf.read_csv(FIXTURE_DIR / "totals.csv", usecols=["order_id", "total"])
    print("totals:")
    print(totals["total"])

    recent = orders.sort_values("amount")
    print("recent (sorted by amount):")
    print(recent["amount"])

    renamed = totals.rename(columns={"total": "gross"})
    print("renamed totals:")
    print(renamed["gross"])

    enriched = totals.assign(net=totals["total"] * 0.9)
    print("enriched totals:")
    print(enriched["net"])

    customers = load_customers()
    joined = orders.merge(customers, on="customer_id")
    print("joined orders+customers:")
    print(joined[["region", "amount"]])

    print("\nAll operations completed against a real cuDF GPU DataFrame.")


if __name__ == "__main__":
    main()

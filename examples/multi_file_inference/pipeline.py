"""Cross-file schema inference — loader column sets propagate into this file.

typedframes builds a project index before checking a directory. Indexing
loaders.py records the inferred column set of every load-and-return
function (load_orders, load_customers) against that function's name, and
indexing transforms.py records the column *requirements* each transform
function has on its first parameter (e.g. contact_label needs "email").

Checking this file resolves the imports and attaches each loader's
inferred schema to the variable it's assigned to. Two kinds of errors
follow, both real bugs this file intentionally contains:

  1. `orders["revenue"]` — direct access to a column not in load_orders'
     inferred set {order_id, customer_id, amount, status}.

  2. `contact_label(customers)` — passes a {customer_id, name, region}
     frame to a function that requires {email, name}. This is exactly the
     kind of bug mypy/ty miss: both sides type-check as plain DataFrames,
     but the columns don't line up, and it would KeyError at runtime.

This file is a static-analysis fixture, not meant to be executed — leave
both errors in place and run `typedframes check examples/multi_file_inference/`
to see them caught.
"""

from __future__ import annotations

from loaders import load_customers, load_orders
from transforms import contact_label, normalise_orders, slim_for_report


def process(path: str) -> None:
    orders = load_orders(path)
    customers = load_customers(path)

    print(orders["order_id"])  # OK - in {order_id, customer_id, amount, status}
    print(orders["revenue"])  # unknown-column - not in load_orders' inferred set

    enriched = normalise_orders(orders)
    report = slim_for_report(orders)
    labeled = contact_label(customers)  # missing-column - customers lacks "email"

    print(enriched)
    print(report)
    print(labeled)

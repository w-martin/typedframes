"""Schema composition example — compose upward, never strip down."""

from typing import Annotated

import pandas as pd

from typedframes import BaseSchema, Column

# -- Compose upward: base schemas first, extend via inheritance ---------------


class UserPublic(BaseSchema):
    """Public user data — the minimal user schema."""

    user_id = Column(type=int)
    email = Column(type=str)
    name = Column(type=str)


class UserFull(UserPublic):
    """Full user record including sensitive data. Extends UserPublic."""

    password_hash = Column(type=str)


class Orders(BaseSchema):
    """Order transaction data."""

    order_id = Column(type=int)
    user_id = Column(type=int)  # same name & type as UserPublic.user_id — OK
    total = Column(type=float)


# -- Combine schemas via multiple inheritance ---------------------------------
# Type checkers see all columns natively — no stubs or plugins needed.


class UserOrders(UserPublic, Orders):
    """Combined schema for merged user/order data."""


# -- Annotate native DataFrames -----------------------------------------------

users: Annotated[pd.DataFrame, UserPublic] = pd.DataFrame(
    {
        "user_id": [1, 2],
        "email": ["a@b.com", "c@d.com"],
        "name": ["Alice", "Bob"],
    }
)

orders: Annotated[pd.DataFrame, Orders] = pd.DataFrame(
    {
        "order_id": [101, 102],
        "user_id": [1, 2],
        "total": [29.99, 49.99],
    }
)

# -- Use the combined schema for merge results --------------------------------
# .s gives the refactor-safe string column name from the descriptor.

merged: Annotated[pd.DataFrame, UserOrders] = users.merge(orders, on=UserPublic.user_id.s)
print("UserOrders columns:", list(UserOrders.columns().keys()))
print(merged)
print()

# -- The + operator also works for quick composition --------------------------

UserOrdersDynamic = UserPublic + Orders
print("Dynamic combination columns:", list(UserOrdersDynamic.columns().keys()))

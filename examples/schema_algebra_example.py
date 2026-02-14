"""Schema composition example — compose upward, never strip down."""

import pandas as pd

from typedframes import BaseSchema, Column
from typedframes.pandas import PandasFrame


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


# -- Wrap raw DataFrames as PandasFrames -------------------------------------

users: PandasFrame[UserPublic] = PandasFrame.from_schema(
    pd.DataFrame(
        {
            "user_id": [1, 2],
            "email": ["a@b.com", "c@d.com"],
            "name": ["Alice", "Bob"],
        }
    ),
    UserPublic,
)

orders: PandasFrame[Orders] = PandasFrame.from_schema(
    pd.DataFrame(
        {
            "order_id": [101, 102],
            "user_id": [1, 2],
            "total": [29.99, 49.99],
        }
    ),
    Orders,
)

# -- Use the combined schema for merge results --------------------------------

merged: PandasFrame[UserOrders] = PandasFrame.from_schema(
    users.merge(orders, on=str(UserPublic.user_id)),
    UserOrders,
)
print("UserOrders columns:", list(UserOrders.columns().keys()))
print(merged)
print()

# -- The + operator also works for quick composition --------------------------

UserOrdersDynamic = UserPublic + Orders
print("Dynamic combination columns:", list(UserOrdersDynamic.columns().keys()))

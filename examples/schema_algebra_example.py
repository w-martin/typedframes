"""Schema algebra example — typing the results of merge, concat, and column subsetting."""

import pandas as pd

from typedframes import BaseSchema, Column, PandasFrame


# -- Define schemas ----------------------------------------------------------


class Users(BaseSchema):
    """User account data."""

    user_id = Column(type=int)
    email = Column(type=str)
    name = Column(type=str)
    password_hash = Column(type=str)


class Orders(BaseSchema):
    """Order transaction data."""

    order_id = Column(type=int)
    user_id = Column(type=int)  # same name & type as Users.user_id — OK
    total = Column(type=float)


# -- Combine schemas with + --------------------------------------------------
# Use when you pd.merge or pd.concat(axis=1) two DataFrames.

UserOrders = Users + Orders
# UserOrders has: user_id, email, name, password_hash, order_id, total

users_df = pd.DataFrame(
    {
        "user_id": [1, 2],
        "email": ["a@b.com", "c@d.com"],
        "name": ["Alice", "Bob"],
        "password_hash": ["hash1", "hash2"],
    }
)

orders_df = pd.DataFrame(
    {
        "order_id": [101, 102],
        "user_id": [1, 2],
        "total": [29.99, 49.99],
    }
)

merged: PandasFrame[UserOrders] = PandasFrame.from_schema(
    users_df.merge(orders_df, on="user_id"),
    UserOrders,
)
print("Merged schema columns:", list(UserOrders.columns().keys()))
print(merged)
print()

# -- Select columns with .select() -------------------------------------------
# Use when you subset a DataFrame to fewer columns.

UserBasic = Users.select([Users.user_id, Users.email])
# UserBasic has: user_id, email

basic: PandasFrame[UserBasic] = PandasFrame.from_schema(
    users_df[["user_id", "email"]],
    UserBasic,
)
print("Selected schema columns:", list(UserBasic.columns().keys()))
print(basic)
print()

# -- Drop columns with .drop() -----------------------------------------------
# Use when you need to exclude sensitive or unwanted columns.

UserPublic = Users.drop([Users.password_hash])
# UserPublic has: user_id, email, name

public: PandasFrame[UserPublic] = PandasFrame.from_schema(
    users_df.drop(columns=["password_hash"]),
    UserPublic,
)
print("Dropped schema columns:", list(UserPublic.columns().keys()))
print(public)

from examples.pandera_example import UserSchema
from typedframes import BaseSchema, Column, ColumnSet
# from typedframes.pandas import PandasFrame
# from typedframes.polars import PolarsFrame

import pandas as pd


class UserSchema(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str, alias="email_address")
    metadata = ColumnSet(members=["age", "gender"])


def main() -> None:
    # Use from_df for example purposes
    # PandasFrame[UserSchema]().from_df(
    #     pd.DataFrame(
    #         {
    #             "user_id": [1],
    #             "email_address": ["a@b.com"],
    #             "age": [20],
    #             "gender": ["m"],
    #         },
    #     ),
    # )
    pass

    # Typo detection on attribute
    # DESIRED: Error: Column 'emai' does not exist in UserFrame
    # (did you mean 'email_address'?)

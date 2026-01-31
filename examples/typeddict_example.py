from typing import TypedDict

from pandas import DataFrame


class UserSchema(TypedDict):
    user_id: int
    email: str


def main() -> None:
    # Use load_users style which is explicitly supported by the linter
    # We use quotes to avoid mypy error: "DataFrame" expects no type arguments
    # Wait, mypy still checks inside quotes if they look like types.
    # To TRULY suppress it without a stub, one might use a TYPE_CHECKING
    # block or # type: ignore[type-arg]
    def load_users() -> "DataFrame[UserSchema]":  # type: ignore[type-arg]
        return DataFrame({"user_id": [1], "email": ["a@b.com"]})

    df: DataFrame[UserSchema] = load_users()  # type: ignore[type-arg]
    # 'name' column doesn't exist in UserSchema
    print(df["name"])  # DESIRED: Error: Column 'name' does not exist in UserSchema

    # Column name typo
    print(df["emai"])
    # DESIRED: Error: Column 'emai' does not exist in UserSchema
    # (did you mean 'email'?)

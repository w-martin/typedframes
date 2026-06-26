"""Narwhals' blind spot: column name errors pass silently through mypy.

This demonstrates that narwhals provides runtime flexibility but no static
type checking for column names. Typos, misspellings, and non-existent columns
are not caught by mypy — they fail only at runtime.
"""

import narwhals as nw


def process_with_typos(df: nw.DataFrame) -> nw.Series:
    """Examples of column errors that mypy does NOT catch with narwhals.

    Args:
        df: A narwhals DataFrame.

    Returns:
        A column (typo: should be 'customer_name').

    This function has multiple column-access errors that mypy will NOT report:
    - 'custmer_name' is a typo (missing 's')
    - 'unit_cost' doesn't exist (should be 'unit_price')
    - 'nonexistent' obviously doesn't exist
    """
    # Typo: should be 'customer_name'
    return df["custmer_name"]


def process_wrong_column_name(df: nw.DataFrame) -> nw.Series:
    """Access non-existent column 'unit_cost' instead of 'unit_price'."""
    # Wrong: should be 'unit_price'
    return df["unit_cost"]


def process_completely_missing(df: nw.DataFrame) -> nw.Series:
    """Access completely non-existent column."""
    # This column doesn't exist anywhere
    return df["nonexistent"]

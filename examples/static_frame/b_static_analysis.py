"""StaticFrame static type checking analysis.

This file shows what mypy can (and cannot) catch with StaticFrame.
Unlike typedframes which provides static column checking via a mypy plugin,
StaticFrame's column access is purely string-based and runtime.
"""

import static_frame as sf


def access_customer_name(frame: sf.Frame) -> sf.Series:
    """Access the customer_name column correctly.

    Args:
        frame: A Frame with customer_name column.

    Returns:
        The customer_name Series.
    """
    return frame["customer_name"]


def access_with_typo(frame: sf.Frame) -> sf.Series:
    """Attempt to access a column with a typo.

    This demonstrates what mypy catches (or doesn't catch) with StaticFrame.
    StaticFrame column access is str-based, not statically typed.

    Args:
        frame: A Frame.

    Returns:
        The custmer_name Series (note the typo).
    """
    # TYPO: 'custmer_name' instead of 'customer_name'
    # mypy does NOT catch this because frame[str] is valid for any string
    return frame["custmer_name"]


def access_nonexistent_column(frame: sf.Frame) -> sf.Series:
    """Attempt to access a nonexistent column.

    Args:
        frame: A Frame.

    Returns:
        The nonexistent Series.
    """
    # mypy does NOT catch this: frame[str] accepts any string argument
    return frame["does_not_exist"]


def schema_mismatch_function(frame: sf.Frame) -> float:
    """Function that expects certain columns.

    Args:
        frame: Must contain 'order_id' and 'total' columns.

    Returns:
        Sum of order IDs and totals.
    """
    # These accesses are NOT statically typed in StaticFrame
    # mypy sees frame[str] -> Series, which is always valid
    return float(frame["order_id"].sum() + frame["total"].sum())


def demonstrate_staticframe_typing() -> None:
    """Show what mypy DOES catch with StaticFrame.

    StaticFrame provides type checking for:
    - Function signatures and return types
    - Series operations (add, multiply, etc.)
    - Method chaining

    It does NOT provide:
    - Column name validation (strings are always valid)
    - Schema mismatches (no schema concept)
    - Type safety for subscript access
    """
    data: dict[str, list[object]] = {
        "order_id": [1, 2, 3],
        "customer_name": ["Alice", "Bob", "Charlie"],
        "product_sku": ["SKU001", "SKU002", "SKU003"],
        "quantity": [5, 3, 10],
        "unit_price": [10.5, 20.0, 15.75],
        "total": [52.5, 60.0, 157.5],
        "shipped": [True, False, True],
    }

    frame = sf.Frame.from_dict(data)  # type: ignore[arg-type]

    # This works:
    customer_names = access_customer_name(frame)
    print(f"Customers: {customer_names}")

    # What about typos? mypy doesn't catch them:
    # typo_result = access_with_typo(frame)  # Would run without error

    # StaticFrame column access is str-based, not statically typed
    print(f"Order IDs: {frame['order_id']}")

    # Typos are not caught at type-check time:
    # shipped_count = frame['shippd'].sum()  # Would fail at RUNTIME, not mypy time

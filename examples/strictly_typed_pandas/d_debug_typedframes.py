"""Debug file to test typedframes column checking."""

from typing import Annotated

import pandas as pd

from typedframes import BaseSchema, Column


class OrderSchema(BaseSchema):
    """Test schema."""

    order_id = Column(type=int)
    customer_name = Column(type=str)


def test_typo() -> None:
    """Test with a typo."""
    orders: Annotated[pd.DataFrame, OrderSchema] = pd.DataFrame(
        {
            "order_id": [1, 2],
            "customer_name": ["Alice", "Bob"],
        }
    )

    # This typo should be caught
    result = orders["custmer_name"]
    print(result)


if __name__ == "__main__":
    test_typo()

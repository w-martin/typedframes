"""SQL column inference for SQLAlchemy: text(), Core select(), and declarative models.

Run: `typedframes check examples/sqlalchemy/`
"""

from pathlib import Path

import pandas as pd
from sqlalchemy import Column, Integer, select, text
from sqlalchemy.orm import DeclarativeBase, Mapped, mapped_column, relationship

# ---------------------------------------------------------------------------
# Declarative models — column sets extracted structurally from __tablename__
# ---------------------------------------------------------------------------


class Base(DeclarativeBase):
    pass


class Order(Base):
    """2.0-style declarative model: Mapped[...] / mapped_column(...)."""

    __tablename__ = "orders"

    id: Mapped[int] = mapped_column(primary_key=True)
    amount: Mapped[float]
    # A relationship attribute is correctly excluded from the inferred column set —
    # it's a Python-side ORM convenience, not a database column.
    items: Mapped[list["OrderItem"]] = relationship()


class OrderItem(Base):
    __tablename__ = "order_items"
    id: Mapped[int] = mapped_column(primary_key=True)
    order_id: Mapped[int] = mapped_column()


class Customer(Base):
    """1.x-style declarative model: plain Column(...) assignments.

    `Column("cust_id", ...)` puts the real DB column name in the first positional
    argument — both `id` (the attribute name) and `cust_id` (the DB name) are
    registered, since callers may write either.
    """

    __tablename__ = "customers"

    id = Column("cust_id", Integer, primary_key=True)
    # "count" is a real column name here — an ORM model can't rename a column the
    # external database actually has, so unlike a typedframes-native BaseSchema, this
    # does NOT raise a reserved-name warning.
    count = Column(Integer)


# ---------------------------------------------------------------------------
# Core select() — resolves against the declarative models' known columns
# ---------------------------------------------------------------------------


def load_via_select_inline() -> None:
    """select(Model.col, ...) resolved directly inline in pd.read_sql."""
    engine = create_engine("postgresql://localhost/mydb")
    df = pd.read_sql(select(Order.id, Order.amount), engine)

    print(df["id"])  # OK
    print(df["amount"])  # OK
    # print(df["items"])  -- unknown-column: not in {id, amount}


def load_via_select_variable_and_label() -> None:
    """select(...) bound to a variable, chained with .where(), with a .label() rename."""
    engine = create_engine("postgresql://localhost/mydb")
    stmt = select(Order.id, Order.amount.label("total")).where(Order.amount > 0)
    df = pd.read_sql(stmt, engine)

    print(df["total"])  # OK -- renamed via .label()
    print(df["id"])  # OK
    # print(df["amount"])  -- unknown-column: renamed to 'total', not present under its
    #   original name


def load_via_select_bare_model_is_untracked() -> None:
    """select(Model) — pulling ALL of a model's columns — is deliberately unsupported.

    The declarative extractor is allowlist-based (Column/mapped_column only) and can
    under-extract on an unusual model definition; treating its output as "the complete
    column set" here would risk a false unknown-column later. Annotate explicitly if
    you need this pattern checked.
    """
    engine = create_engine("postgresql://localhost/mydb")
    df = pd.read_sql(select(Order), engine)  # untracked-dataframe
    print(df)


def load_via_text_literal() -> None:
    """text(...) wrapping a literal SQL string.

    The idiomatic way to pass raw SQL through a SQLAlchemy engine.
    """
    engine = create_engine("postgresql://localhost/mydb")
    df = pd.read_sql(text("SELECT cust_id, count FROM customers"), engine)

    print(df["cust_id"])  # OK
    # print(df["region"])  -- unknown-column: not in {cust_id, count}


def load_via_text_from_file() -> None:
    """SQL kept in a .sql file, wrapped in text().

    Combines file tracing with the text() unwrapping above.
    """
    engine = create_engine("postgresql://localhost/mydb")
    sql_text = (Path(__file__).parent / "orders.sql").read_text()
    df = pd.read_sql(text(sql_text), engine)

    print(df["order_id"])  # OK -- orders.sql selects order_id, amount
    # print(df["cust_id"])  -- unknown-column: not in {order_id, amount}

use super::*;

#[test]
fn test_declarative_model_20_style_registers_mapped_columns() {
    let source = r#"
from sqlalchemy.orm import Mapped, mapped_column, DeclarativeBase

class Base(DeclarativeBase):
    pass

class Order(Base):
    __tablename__ = "orders"
    id: Mapped[int] = mapped_column(primary_key=True)
    amount: Mapped[float]
    customer_items: Mapped[list["Item"]]
"#;
    let mut linter = Linter::new();
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    let cols = linter
        .schemas
        .get("Order")
        .expect("Order should be registered");
    assert!(cols.contains(&"id".to_string()), "cols: {cols:?}");
    assert!(cols.contains(&"amount".to_string()), "cols: {cols:?}");
    assert!(
        !cols.contains(&"customer_items".to_string()),
        "relationship attribute should be excluded: {cols:?}"
    );
    assert!(
        !cols.contains(&"__tablename__".to_string()),
        "dunder should be excluded: {cols:?}"
    );
}

#[test]
fn test_declarative_model_legacy_column_style_and_positional_db_name() {
    let source = r#"
from sqlalchemy import Column, Integer, String
from sqlalchemy.orm import declarative_base, relationship

Base = declarative_base()

class Order(Base):
    __tablename__ = "orders"
    id = Column("order_id", Integer, primary_key=True)
    amount = Column(Integer)
    customer = relationship("Customer")
"#;
    let mut linter = Linter::new();
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    let cols = linter
        .schemas
        .get("Order")
        .expect("Order should be registered");
    // Both the attribute name and the positional DB-name override are registered.
    assert!(cols.contains(&"id".to_string()), "cols: {cols:?}");
    assert!(cols.contains(&"order_id".to_string()), "cols: {cols:?}");
    assert!(cols.contains(&"amount".to_string()), "cols: {cols:?}");
    assert!(
        !cols.contains(&"customer".to_string()),
        "relationship() should be excluded: {cols:?}"
    );
}

#[test]
fn test_orm_model_skips_reserved_method_name_check() {
    // "count" would trip CODE_RESERVED_NAME on a typedframes-native BaseSchema, but
    // an ORM model's column names come from an external database the user doesn't
    // control renaming — the check must not fire here.
    let source = r#"
from sqlalchemy.orm import Mapped, mapped_column

class Order(Base):
    __tablename__ = "orders"
    id: Mapped[int] = mapped_column(primary_key=True)
    count: Mapped[int]
"#;
    let mut linter = Linter::new();
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    assert!(
        errors.iter().all(|e| e.code != CODE_RESERVED_NAME),
        "errors: {errors:?}"
    );
}

#[test]
fn test_select_columns_resolve_inline_in_read_sql() {
    let source = r#"
import pandas as pd
from sqlalchemy import select
from sqlalchemy.orm import Mapped, mapped_column

class Order(Base):
    __tablename__ = "orders"
    id: Mapped[int] = mapped_column(primary_key=True)
    amount: Mapped[float]

df = pd.read_sql(select(Order.id, Order.amount), engine)
print(df["id"])
print(df["missing"])
"#;
    let mut linter = Linter::new();
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("missing"));
}

#[test]
fn test_select_columns_resolve_through_a_bound_variable_and_label() {
    let source = r#"
import pandas as pd
from sqlalchemy import select
from sqlalchemy.orm import Mapped, mapped_column

class Order(Base):
    __tablename__ = "orders"
    id: Mapped[int] = mapped_column(primary_key=True)
    amount: Mapped[float]

stmt = select(Order.id, Order.amount.label("total")).where(Order.amount > 0)
df = pd.read_sql(stmt, engine)
print(df["total"])
print(df["missing"])
"#;
    let mut linter = Linter::new();
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("missing"));
}

#[test]
fn test_bare_select_of_model_is_unresolved() {
    // select(Order) — pulling "all" of a model's columns — is deliberately not
    // supported: the ORM extractor is allowlist-based and can under-extract on an
    // unusual declarative pattern, so treating its output as "the complete column
    // set" here would risk a false unknown-column later.
    let source = r#"
import pandas as pd
from sqlalchemy import select
from sqlalchemy.orm import Mapped, mapped_column

class Order(Base):
    __tablename__ = "orders"
    id: Mapped[int] = mapped_column(primary_key=True)

df = pd.read_sql(select(Order), engine)
"#;
    let mut linter = Linter::new();
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNTRACKED_DATAFRAME);
}

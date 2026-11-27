use super::*;

#[test]
fn test_should_count_typed_dataframe_for_schema_from_pandas() {
    // arrange
    let source = r#"
import pandas as pd
from typedframes import BaseSchema, Column

class UserSchema(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)

raw = pd.read_csv("users.csv")
df = UserSchema.from_pandas(raw)
print(df["user_id"])
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert: the bare `raw = pd.read_csv(...)` load call is untyped, but the
    // `UserSchema.from_pandas(raw)` assignment is a second, typed DataFrame origin
    assert_eq!(linter.dataframes_total, 2);
    assert_eq!(linter.dataframes_typed, 1);
}

#[test]
fn test_should_count_typed_dataframe_for_function_return_schema() {
    // arrange
    let source = r#"
from typedframes import BaseSchema, Column

class UserSchema(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)

def load_users() -> DataFrame[UserSchema]:
    return pd.read_csv("users.csv")

df = load_users()
print(df["user_id"])
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert: `df = load_users()` resolves via the known return schema
    assert_eq!(linter.dataframes_total, 1);
    assert_eq!(linter.dataframes_typed, 1);
}

#[test]
fn test_should_count_typed_dataframe_for_annotated_assignment() {
    // An `Annotated[pd.DataFrame, Schema]`-annotated variable has a fully known
    // schema by construction -- the annotation IS the resolution -- so it must
    // count toward coverage even though the RHS itself (a bare `load()` call) is
    // not a recognized load pattern.
    let source = r#"
from typing import Annotated
import pandas as pd
from typedframes import BaseSchema, Column

class UserSchema(BaseSchema):
    user_id = Column(type=int)

df: Annotated[pd.DataFrame, UserSchema] = load()
print(df["missing"])
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(linter.dataframes_total, 1);
    assert_eq!(linter.dataframes_typed, 1);
}

#[test]
fn test_should_count_typed_dataframe_for_bracket_annotated_assignment() {
    let source = r#"
from typedframes import BaseSchema, Column

class UserSchema(BaseSchema):
    user_id = Column(type=int)

df: DataFrame[UserSchema] = load()
print(df["missing"])
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(linter.dataframes_total, 1);
    assert_eq!(linter.dataframes_typed, 1);
}

#[test]
fn test_should_count_typed_dataframe_for_quoted_annotated_assignment() {
    let source = r#"
from typedframes import BaseSchema, Column

class UserSchema(BaseSchema):
    user_id = Column(type=int)

df: "DataFrame[UserSchema]" = load()
print(df["missing"])
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(linter.dataframes_total, 1);
    assert_eq!(linter.dataframes_typed, 1);
}

#[test]
fn test_should_not_double_count_annotated_assignment_whose_value_also_resolves() {
    // The annotation (`DataFrame[UserSchema]`) and the value
    // (`DataFrame[UserSchema](...)` instantiation) both independently resolve the
    // same variable's schema -- this must still count as ONE DataFrame origin, not
    // two, since it's one statement producing one DataFrame.
    let source = r#"
from typedframes import BaseSchema, Column

class UserSchema(BaseSchema):
    user_id = Column(type=int)

df: DataFrame[UserSchema] = DataFrame[UserSchema](data)
print(df["missing"])
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(linter.dataframes_total, 1);
    assert_eq!(linter.dataframes_typed, 1);
}

#[test]
fn test_should_count_typed_dataframe_for_bracket_instantiation() {
    let source = r#"
from typedframes import BaseSchema, Column

class UserSchema(BaseSchema):
    user_id = Column(type=int)

df = DataFrame[UserSchema](data)
print(df["missing"])
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(linter.dataframes_total, 1);
    assert_eq!(linter.dataframes_typed, 1);
}

#[test]
fn test_should_count_typed_dataframe_for_schema_constructor_method_chain() {
    let source = r#"
from typedframes import BaseSchema, Column

class UserSchema(BaseSchema):
    user_id = Column(type=int)

df = UserSchema().read_csv("users.csv")
print(df["missing"])
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(linter.dataframes_total, 1);
    assert_eq!(linter.dataframes_typed, 1);
}

#[test]
fn test_should_count_typed_dataframe_for_merge_result() {
    let source = r#"
from typedframes import BaseSchema, Column

class UserSchema(BaseSchema):
    user_id = Column(type=int)

class OrderSchema(BaseSchema):
    order_id = Column(type=int)

users: DataFrame[UserSchema] = load()
orders: DataFrame[OrderSchema] = load()
merged = users.merge(orders)
print(merged["missing"])
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert: 2 from the annotated `users`/`orders` assignments, 1 more for `merged`
    assert_eq!(linter.dataframes_total, 3);
    assert_eq!(linter.dataframes_typed, 3);
}

#[test]
fn test_should_not_count_unresolved_function_call_assignment() {
    // arrange: `compute()` is not known to return a DataFrame at all, so it must
    // not inflate the coverage denominator
    let source = r#"
def compute():
    return 42

x = compute()
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(linter.dataframes_total, 0);
    assert_eq!(linter.dataframes_typed, 0);
}

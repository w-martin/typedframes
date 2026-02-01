use _rust_linter::Linter;
use std::fs;
use tempfile::tempdir;

#[test]
fn test_should_detect_missing_column_with_base_schema() {
    // arrange
    let mut linter = Linter::new();
    let source = r#"
from typedframes import BaseSchema, Column

class UserSchema(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)

def main():
    df: DataFrame[UserSchema] = load()
    print(df["user_id"])
    print(df["non_existent"])
"#;
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("test.py");
    fs::write(&file_path, source).unwrap();

    // act
    let errors = linter.check_file_internal(source, &file_path).unwrap();

    // assert
    assert!(!errors.is_empty(), "Should have detected errors");
    assert!(errors.iter().any(|e| e
        .message
        .contains("Column 'non_existent' does not exist in UserSchema")));
}

#[test]
fn test_should_suggest_typo_correction() {
    // arrange
    let mut linter = Linter::new();
    let source = r#"
from typedframes import BaseSchema, Column

class UserSchema(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)

def main():
    df: DataFrame[UserSchema] = load()
    print(df["emai"])
"#;
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("test.py");
    fs::write(&file_path, source).unwrap();

    // act
    let errors = linter.check_file_internal(source, &file_path).unwrap();

    // assert
    assert!(!errors.is_empty());
    assert!(errors
        .iter()
        .any(|e| e.message.contains("did you mean 'email'?")));
}

#[test]
fn test_should_support_column_alias() {
    // arrange
    let mut linter = Linter::new();
    let source = r#"
from typedframes import BaseSchema, Column

class UserSchema(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str, alias="user_email")

def main():
    df: DataFrame[UserSchema] = load()
    print(df["user_email"])  # OK - alias
    print(df["email"])       # Error - use alias
"#;
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("test.py");
    fs::write(&file_path, source).unwrap();

    // act
    let errors = linter.check_file_internal(source, &file_path).unwrap();

    // assert
    assert!(!errors.is_empty());
    // email (without alias) should be an error
    assert!(errors.iter().any(|e| e
        .message
        .contains("Column 'email' does not exist in UserSchema")));
    // user_email should be valid
    assert!(!errors.iter().any(|e| e
        .message
        .contains("Column 'user_email' does not exist")));
}

#[test]
fn test_should_support_column_set() {
    // arrange
    let mut linter = Linter::new();
    let source = r#"
from typedframes import BaseSchema, Column, ColumnSet

class SensorSchema(BaseSchema):
    timestamp = Column(type=str)
    temperatures = ColumnSet(members=["temp_1", "temp_2", "temp_3"], type=float)

def main():
    df: DataFrame[SensorSchema] = load()
    print(df["timestamp"])  # OK
    print(df["temp_1"])     # OK - ColumnSet member
    print(df["temp_4"])     # Error - not in members
"#;
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("test.py");
    fs::write(&file_path, source).unwrap();

    // act
    let errors = linter.check_file_internal(source, &file_path).unwrap();

    // assert
    assert!(!errors.is_empty());
    assert!(errors.iter().any(|e| e
        .message
        .contains("Column 'temp_4' does not exist in SensorSchema")));
    assert!(!errors.iter().any(|e| e
        .message
        .contains("Column 'temp_1' does not exist")));
}

#[test]
fn test_should_support_annotated_polars_pattern() {
    // arrange
    let mut linter = Linter::new();
    let source = r#"
from typing import Annotated
import polars as pl
from typedframes import BaseSchema, Column

class UserSchema(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)

def main():
    df: Annotated[pl.DataFrame, UserSchema] = pl.read_csv("data.csv")
    print(df["user_id"])       # OK
    print(df["wrong_column"])  # Error
"#;
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("test.py");
    fs::write(&file_path, source).unwrap();

    // act
    let errors = linter.check_file_internal(source, &file_path).unwrap();

    // assert
    assert!(!errors.is_empty());
    assert!(errors.iter().any(|e| e
        .message
        .contains("Column 'wrong_column' does not exist in UserSchema")));
}

#[test]
fn test_should_support_pandas_frame_subscript() {
    // arrange
    let mut linter = Linter::new();
    let source = r#"
from typedframes import BaseSchema, Column, PandasFrame

class UserSchema(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)

def main():
    df: PandasFrame[UserSchema] = load()
    print(df["user_id"])  # OK
    print(df["missing"])  # Error
"#;
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("test.py");
    fs::write(&file_path, source).unwrap();

    // act
    let errors = linter.check_file_internal(source, &file_path).unwrap();

    // assert
    assert!(!errors.is_empty());
    assert!(errors.iter().any(|e| e
        .message
        .contains("Column 'missing' does not exist in UserSchema")));
}

#[test]
fn test_should_support_polars_frame_subscript() {
    // arrange
    let mut linter = Linter::new();
    let source = r#"
from typedframes import BaseSchema, Column, PolarsFrame

class UserSchema(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)

def main():
    df: PolarsFrame[UserSchema] = load()
    print(df["user_id"])  # OK
    print(df["missing"])  # Error
"#;
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("test.py");
    fs::write(&file_path, source).unwrap();

    // act
    let errors = linter.check_file_internal(source, &file_path).unwrap();

    // assert
    assert!(!errors.is_empty());
    assert!(errors.iter().any(|e| e
        .message
        .contains("Column 'missing' does not exist in UserSchema")));
}

#[test]
fn test_should_support_merge_and_concat() {
    // arrange
    let mut linter = Linter::new();
    let source = r#"
import pandas as pd
from typedframes import BaseSchema, Column

class UserSchema(BaseSchema):
    user_id = Column(type=int)
    name = Column(type=str)

class OrderSchema(BaseSchema):
    order_id = Column(type=int)
    user_id = Column(type=int)
    amount = Column(type=float)

def main():
    users: DataFrame[UserSchema] = pd.DataFrame({"user_id": [1], "name": ["Alice"]})
    orders: DataFrame[OrderSchema] = pd.DataFrame({"order_id": [101], "user_id": [1], "amount": [50.0]})

    # Test merge
    merged = users.merge(orders)
    print(merged.name)     # OK
    print(merged.amount)   # OK
    print(merged.missing)  # Error

    # Test concat
    concatenated = pd.concat([users, orders])
    print(concatenated.name)   # OK
    print(concatenated.amount) # OK
    print(concatenated.typo)   # Error
"#;
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("test_merge.py");
    fs::write(&file_path, source).unwrap();

    // act
    let errors = linter.check_file_internal(source, &file_path).unwrap();

    // assert
    assert!(!errors.is_empty(), "Should have detected errors");
    assert!(errors.iter().any(|e| e
        .message
        .contains("Column 'missing' does not exist in UserSchema_OrderSchema")));
    assert!(errors.iter().any(|e| e
        .message
        .contains("Column 'typo' does not exist in UserSchema_OrderSchema")));
}

#[test]
fn test_should_track_mutations() {
    // arrange
    let mut linter = Linter::new();
    let source = r#"
from typedframes import BaseSchema, Column

class UserSchema(BaseSchema):
    user_id = Column(type=int)

def main():
    df: DataFrame[UserSchema] = load()
    df["new_column"] = 123
"#;
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("test_mut.py");
    fs::write(&file_path, source).unwrap();

    // act
    let errors = linter.check_file_internal(source, &file_path).unwrap();

    // assert
    assert!(!errors.is_empty());
    assert!(errors
        .iter()
        .any(|e| e.message.contains("mutation tracking")));
}

#[test]
fn test_should_support_schema_from_pandas_pattern() {
    // arrange
    let mut linter = Linter::new();
    let source = r#"
from typedframes import BaseSchema, Column
import pandas as pd

class UserSchema(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)

def main():
    raw_df = pd.read_csv("data.csv")
    df = UserSchema.from_pandas(raw_df)
    print(df["user_id"])  # OK
    print(df["missing"])  # Error
"#;
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("test.py");
    fs::write(&file_path, source).unwrap();

    // act
    let errors = linter.check_file_internal(source, &file_path).unwrap();

    // assert
    assert!(!errors.is_empty());
    assert!(errors.iter().any(|e| e
        .message
        .contains("Column 'missing' does not exist in UserSchema")));
}

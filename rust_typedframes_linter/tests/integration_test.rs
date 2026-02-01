use rust_pandas_linter::Linter;
use std::fs;
use tempfile::tempdir;

#[test]
fn test_should_detect_missing_column() {
    // arrange
    let mut linter = Linter::new();
    let source = r#"
import pandera as pa
from pandera.typing import DataFrame, Series

class UserSchema(pa.DataFrameModel):
    user_id: Series[int]
    email: Series[str]

def main():
    df = DataFrame[UserSchema]({"user_id": [1], "email": ["test@example.com"]})
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
import pandera as pa
from pandera.typing import DataFrame, Series

class UserSchema(pa.DataFrameModel):
    user_id: Series[int]
    email: Series[str]

def main():
    df = DataFrame[UserSchema]({"user_id": [1], "email": ["test@example.com"]})
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
fn test_should_support_typed_dict() {
    // arrange
    let mut linter = Linter::new();
    let source = r#"
from typing import TypedDict
from pandas import DataFrame

class UserDict(TypedDict):
    user_id: int
    email: str

def main():
    df = DataFrame[UserDict]({"user_id": [1], "email": ["test@example.com"]})
    print(df["missing"])
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
        .contains("Column 'missing' does not exist in UserDict")));
}

#[test]
fn test_should_support_pandandic_enhanced() {
    // arrange
    let mut linter = Linter::new();
    let source = r#"
from pandandic import BaseFrame, Column, ColumnSet, ColumnGroup

class MyFrame(BaseFrame):
    foo = Column(type=str)
    bar = Column(type=int, alias="BAR")
    multi = ColumnSet(members=["col1", "col2"])
    grp = ColumnGroup(members=[foo, multi])

def main():
    df = MyFrame().read_csv("test.csv")
    print(df.foo)       # OK
    print(df.BAR)       # OK (alias)
    print(df.col1)      # OK (ColumnSet member)
    print(df.missing)   # Error
    print(df["foo"])    # OK
    print(df["BAR"])    # OK
    print(df["missing"]) # Error
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
        .contains("Column 'missing' does not exist in MyFrame")));
    // Check that aliases and members are recognized
    assert!(
        !errors
            .iter()
            .any(|e| e.message.contains("'foo'") && e.message.contains("does not exist")),
        "foo should exist"
    );
    assert!(
        !errors
            .iter()
            .any(|e| e.message.contains("'BAR'") && e.message.contains("does not exist")),
        "BAR should exist"
    );
    assert!(
        !errors
            .iter()
            .any(|e| e.message.contains("'col1'") && e.message.contains("does not exist")),
        "col1 should exist"
    );
}

#[test]
fn test_should_support_pandandic() {
    // arrange
    let mut linter = Linter::new();
    let source = r#"
from pandas import DataFrame

class UserSchema(DataFrame):
    user_id: int
    email: str

def main():
    df = UserSchema({"user_id": [1], "email": ["test@example.com"]})
    # Note: Linter currently tracks variable assignment if it looks like DataFrame[Schema]
    # or load_users() call. For pandandic subclassing, we might need more logic if it's df = UserSchema(...)
    # Let's see if our current logic handles df: DataFrame[UserSchema] = ...
    df2: DataFrame[UserSchema] = DataFrame[UserSchema]({"user_id": [1]})
    print(df2["missing"])
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
from typing import TypedDict
from pandas import DataFrame

class UserSchema(TypedDict):
    user_id: int
    name: str

class OrderSchema(TypedDict):
    order_id: int
    user_id: int
    amount: float

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
from pandera.typing import DataFrame, Series
import pandera as pa

class UserSchema(pa.DataFrameModel):
    user_id: Series[int]

def main():
    df = DataFrame[UserSchema]({"user_id": [1]})
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

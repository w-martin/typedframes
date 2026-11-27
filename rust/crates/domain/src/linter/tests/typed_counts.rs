use super::*;

#[test]
fn test_should_lint_annotated_polars_pattern() {
    // arrange
    let source = r#"
from typing import Annotated
import polars as pl
from typedframes import BaseSchema, Column

class UserSchema(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)

df: Annotated[pl.DataFrame, UserSchema] = pl.read_csv("data.csv")
print(df["user_id"])
print(df["wrong_column"])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(errors.len(), 1);
    assert!(errors[0].message.contains("wrong_column"));
    assert!(errors[0].message.contains("UserSchema"));
}

#[test]
fn test_should_track_function_return_type() {
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
print(df["name"])
print(df["emai"])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(errors.len(), 2);
    assert!(errors[0].message.contains("name"));
    assert!(errors[0].message.contains("UserSchema"));
    assert!(errors[1].message.contains("emai"));
    assert!(errors[1].message.contains("did you mean 'email'"));
}

#[test]
fn test_should_count_typed_dataframe_for_load_call_with_usecols() {
    // arrange
    let source = r#"
import pandas as pd

df = pd.read_csv("data.csv", usecols=["a", "b"])
print(df["a"])
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
fn test_should_count_typed_dataframe_for_constructor_with_columns_kwarg() {
    // arrange: columns= is the DataFrame() constructor's own analog of read_csv's
    // usecols= -- same recognition path, reused via LOAD_FUNCTIONS.
    let source = r#"
import pandas as pd

df = pd.DataFrame(data, columns=["a", "b"])
print(df["a"])
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
fn test_should_infer_columns_from_a_literal_list_of_dict_literals() {
    // arrange: pandas' "records" orientation -- one dict literal per row, passed
    // directly as a list literal. Columns are the union of every row's keys.
    let source = r#"
import pandas as pd

df = pd.DataFrame([{"a": 1, "b": 2}, {"a": 3, "c": 4}])
print(df["a"])
print(df["c"])
print(df["nope"])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert: union is {a, b, c}, in first-seen order; only the genuine typo flags.
    assert_eq!(linter.dataframes_total, 1);
    assert_eq!(linter.dataframes_typed, 1);
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'nope'"));
    assert!(errors[0].message.contains("{a, b, c}"));
}

#[test]
fn test_should_leave_a_list_of_non_dict_elements_untracked() {
    // arrange: not every element is a dict literal -- can't infer columns from it.
    let source = r#"
import pandas as pd

df = pd.DataFrame([{"a": 1}, "not a dict"])
print(df["a"])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert: recognized as a DataFrame origin, but not typed -- no false claim.
    assert_eq!(linter.dataframes_total, 1);
    assert_eq!(linter.dataframes_typed, 0);
    assert!(
        errors
            .iter()
            .all(|e| e.code != CODE_UNKNOWN_COLUMN && e.code != CODE_UNVERIFIABLE_COLUMN),
        "errors: {errors:?}"
    );
}

#[test]
fn test_should_not_tell_a_call_that_already_passes_columns_to_specify_columns() {
    // arrange: `columns=` is given, but its elements can't be resolved statically.
    let source = r#"
import pandas as pd


class C:
    def f(self):
        df = pd.DataFrame(columns=[self.a, self.b])
        return df["a"]
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    let hint = errors
        .iter()
        .find(|e| e.code == CODE_UNTRACKED_DATAFRAME)
        .expect("an untracked warning");
    assert!(
        hint.message.contains("isn't made of string literals"),
        "{}",
        hint.message
    );
    assert!(
        !hint.message.contains("or specify `columns=`"),
        "{}",
        hint.message
    );
}

#[test]
fn test_should_still_suggest_columns_when_a_dataframe_is_built_without_any() {
    // arrange
    let source = "import pandas as pd\n\n\ndef f(rows):\n    df = pd.DataFrame(rows)\n    return df[\"a\"]\n";
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    let hint = errors
        .iter()
        .find(|e| e.code == CODE_UNTRACKED_DATAFRAME)
        .expect("an untracked warning");
    assert!(
        hint.message.contains("specify `columns=`"),
        "{}",
        hint.message
    );
}

use super::*;

#[test]
fn test_should_fold_columns_via_rename_with_str_lower() {
    // A Snowflake result genuinely has upper-cased columns; .rename(columns=str.lower)
    // -- a callable, not a literal dict -- should fold the whole known column set to
    // lower case, not be silently treated as a no-op.
    let source = r#"
import pandas as pd

df = pd.read_sql("SELECT order_id, amount FROM orders", conn)
lowered = df.rename(columns=str.lower)
print(lowered["order_id"])
print(lowered["ORDER_ID"])
"#;
    let mut linter = Linter::new();
    linter.fs = Arc::new(OsFiles).into();
    linter.with_context(
        PathBuf::from("/project"),
        &LinterConfig {
            enabled: None,
            warnings: None,
            sql_dialect: Some("snowflake".to_string()),
            trace_external_packages: None,
            excluded_external_packages: None,
            exclude: None,
        },
    );

    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
    assert!(errors[0].message.contains("ORDER_ID"));
}

#[test]
fn test_should_fold_columns_via_columns_attribute_assignment() {
    // df.columns = df.columns.str.lower() -- an attribute-assignment target, not a
    // method-chain call -- should be recognized the same way as the rename() form.
    let source = r#"
import pandas as pd

df = pd.read_sql("SELECT order_id, amount FROM orders", conn)
df.columns = df.columns.str.lower()
print(df["order_id"])
print(df["ORDER_ID"])
"#;
    let mut linter = Linter::new();
    linter.fs = Arc::new(OsFiles).into();
    linter.with_context(
        PathBuf::from("/project"),
        &LinterConfig {
            enabled: None,
            warnings: None,
            sql_dialect: Some("snowflake".to_string()),
            trace_external_packages: None,
            excluded_external_packages: None,
            exclude: None,
        },
    );

    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
    assert!(errors[0].message.contains("ORDER_ID"));
}

#[test]
fn test_should_mark_unresolved_for_unrecognized_transform_function() {
    // A custom/arbitrary transform function is NOT reverse-engineered -- and
    // unlike an unrecognized shape simply being ignored, `rename()` IS a known
    // schema-changing method, so its result becomes its own new, unresolved
    // origin rather than silently keeping the pre-rename schema (which risks
    // validating access against columns that are now wrong).
    let source = r#"
import pandas as pd
import my_internal_pkg

df = pd.read_sql("SELECT order_id, amount FROM orders", conn)
lowered = df.rename(columns=my_internal_pkg.normalize)
print(lowered["ORDER_ID"])
"#;
    let mut linter = Linter::new();
    linter.fs = Arc::new(OsFiles).into();
    linter.with_context(
        PathBuf::from("/project"),
        &LinterConfig {
            enabled: None,
            warnings: None,
            sql_dialect: Some("snowflake".to_string()),
            trace_external_packages: None,
            excluded_external_packages: None,
            exclude: None,
        },
    );

    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // `lowered` is now its own unresolved origin -- ANY access on it is
    // unverifiable, not validated against the (possibly stale) pre-rename schema.
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
    assert!(errors[0].message.contains("ORDER_ID"));
}

#[test]
fn test_should_count_rename_as_a_new_leg_in_the_same_scope() {
    // arrange: no function call at all -- a schema-changing mutation is its own
    // counted origin even within one plain, linear scope (see
    // `count_mutation_leg`'s doc comment).
    let source = r#"
import pandas as pd

df = pd.read_csv("x.csv", usecols=["a", "b"])
df = df.rename(columns={"a": "aa"})
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(linter.dataframes_total, 2);
    assert_eq!(linter.dataframes_typed, 2);
}

#[test]
fn test_should_count_drop_as_a_new_leg_in_the_same_scope() {
    // arrange
    let source = r#"
import pandas as pd

df = pd.read_csv("x.csv", usecols=["a", "b"])
df = df.drop(columns=["a"])
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(linter.dataframes_total, 2);
    assert_eq!(linter.dataframes_typed, 2);
}

#[test]
fn test_should_count_assign_as_a_new_leg_in_the_same_scope() {
    // arrange
    let source = r#"
import pandas as pd

df = pd.read_csv("x.csv", usecols=["a", "b"])
df = df.assign(c=1)
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(linter.dataframes_total, 2);
    assert_eq!(linter.dataframes_typed, 2);
}

#[test]
fn test_should_count_dynamic_drop_as_its_own_untracked_leg() {
    // arrange: `.drop()` IS a known schema-changing method, so a dynamic
    // (non-literal) column list still counts as a new origin -- just untracked,
    // rather than silently keeping the pre-drop schema (which would validate
    // `df["a"]` against columns that may no longer be there).
    let source = r#"
import pandas as pd

cols_to_drop = compute_cols()
df = pd.read_csv("x.csv", usecols=["a", "b"])
df = df.drop(columns=cols_to_drop)
print(df["a"])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(linter.dataframes_total, 2);
    assert_eq!(linter.dataframes_typed, 1);
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
}

#[test]
fn test_should_propagate_unresolved_base_through_rename_without_false_positive() {
    // arrange: the base (`get_unresolved()`, a bare `-> pd.DataFrame` return with
    // no attached Schema) is unresolved BEFORE the rename ever runs -- an empty
    // placeholder column list, not a confirmed-empty schema. Regression coverage
    // for a real bug this same change surfaced: the rename mapping's
    // old-column-exists check used to run unconditionally against that empty
    // placeholder, falsely reporting every renamed column as nonexistent.
    let source = r#"
import pandas as pd


def get_unresolved() -> pd.DataFrame:
    return pd.read_sql(some_dynamic_query, conn)


df = get_unresolved()
df = df.rename(columns={"a": "aa"})
print(df["aa"])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert: no false unknown-column for 'a' -- only the real, expected
    // unverifiable-column for accessing an unresolved result.
    let unverifiable = errors
        .iter()
        .filter(|e| e.code == CODE_UNVERIFIABLE_COLUMN)
        .count();
    assert_eq!(unverifiable, 1, "errors: {errors:?}");
    assert!(errors.iter().all(|e| e.code != CODE_UNKNOWN_COLUMN));
    assert_eq!(linter.dataframes_typed, 0);
}

use super::*;

#[test]
fn test_should_track_column_list_slice_as_requirement() {
    // arrange: preproc narrows df to {a, b} via a list-slice. That narrowing
    // itself is a genuine requirement on df's caller (a, b must be present).
    let source = r#"
import pandas as pd

def preproc(df: pd.DataFrame) -> pd.DataFrame:
    slim = df[["a", "b"]]
    return slim
"#;
    let mut linter = Linter::new();
    let _ = linter.check_file_internal(source, Path::new("preproc.py"));

    // assert
    let (required, _) = linter
        .requires
        .get("preproc")
        .expect("preproc should have a recorded requirement");
    assert_eq!(required, &vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn test_should_not_leak_narrowed_variable_misuse_into_caller_requirement() {
    // arrange: `slim = df[["a", "b"]]` gives slim a fully known schema {a, b}
    // (via the existing multi-column-subscript tracking) — a later `slim["c"]`
    // is therefore a certain, local bug in preproc itself, not an ambiguous
    // "maybe missing from the caller" requirement. It must be caught as a
    // direct unknown-column error, and must NOT also inflate preproc's
    // caller-facing contract to {a, b, c} — df's caller only ever needs to
    // supply {a, b}; 'c' is preproc's own mistake in how it uses slim.
    let source = r#"
import pandas as pd

def preproc(df: pd.DataFrame) -> pd.DataFrame:
    slim = df[["a", "b"]]
    z = slim["c"]
    return slim
"#;
    let mut linter = Linter::new();
    let errors = linter
        .check_file_internal(source, Path::new("preproc.py"))
        .unwrap();

    // assert: caught locally as a certain bug ...
    assert_eq!(errors.len(), 1, "expected one error, got: {errors:?}");
    assert_eq!(errors[0].code, "unknown-column");
    assert!(errors[0].message.contains("'c'"));
    assert!(errors[0].message.contains("{a, b}"));

    // ... and NOT folded into the ambiguous caller-facing contract.
    let (required, _) = linter
        .requires
        .get("preproc")
        .expect("preproc should have a recorded requirement");
    assert_eq!(required, &vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn test_should_resolve_transitive_requires_with_list_slice_delegate() {
    // arrange: same delegate chain as
    // test_should_resolve_transitive_requires_through_delegate_chain, but
    // preproc uses a list-slice (`df[["a"]]`) instead of a single-string
    // subscript — this is the exact pattern that originally slipped through
    // the direct-requirement scan.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    fs::write(
        root.join("loaders.py"),
        r#"
import pandas as pd

def load(path: str) -> pd.DataFrame:
    df = pd.read_csv(path, usecols=["a", "b"])
    return df
"#,
    )
    .unwrap();
    fs::write(
        root.join("steps.py"),
        r#"
import pandas as pd

def preproc(df: pd.DataFrame) -> pd.DataFrame:
    return df[["a"]]

def infer(df: pd.DataFrame) -> pd.DataFrame:
    x = df["b"]
    return df

def postproc(df: pd.DataFrame) -> pd.DataFrame:
    y = df["c"]
    return df

def transform(df: pd.DataFrame) -> pd.DataFrame:
    step1 = preproc(df)
    step2 = infer(step1)
    step3 = postproc(step2)
    return step3
"#,
    )
    .unwrap();
    let pipeline_source = r#"
from loaders import load
from steps import transform

def process(path: str) -> None:
    df = load(path)
    result = transform(df)
    print(result)
"#;
    fs::write(root.join("pipeline.py"), pipeline_source).unwrap();

    // act
    let index = build_index_internal(root);
    let mut linter = Linter::new();
    let pipeline_path = root.join("pipeline.py");
    linter.load_cross_file_symbols(&index, pipeline_source, &pipeline_path, root);
    let errors = linter
        .check_file_internal(pipeline_source, &pipeline_path)
        .unwrap();

    // assert: 'a' now correctly contributes via preproc's list-slice, alongside
    // 'b' and 'c' from infer/postproc.
    assert_eq!(errors.len(), 1, "expected one error, got: {errors:?}");
    assert_eq!(errors[0].code, "missing-column");
    assert!(errors[0].message.contains("required: {a, b, c}"));
}

#[test]
fn test_with_context_sets_dialect_from_config() {
    let mut linter = Linter::new();
    assert_eq!(linter.sql_dialect, sql::SqlDialect::Generic);

    let config = LinterConfig {
        enabled: None,
        warnings: None,
        sql_dialect: Some("snowflake".to_string()),
        trace_external_packages: None,
        excluded_external_packages: None,
        exclude: None,
    };
    linter.with_context(PathBuf::from("/project"), &config);
    assert_eq!(linter.sql_dialect, sql::SqlDialect::Snowflake);
    assert_eq!(linter.project_root, Some(PathBuf::from("/project")));
}

#[test]
fn test_snowflake_dialect_folds_read_sql_columns_to_uppercase_and_flags_lowercase_access() {
    // A Snowflake connection genuinely returns ORDER_ID (uppercase) for an unquoted
    // `order_id` in the SELECT list — see sql::SqlDialect::fold_case. Once
    // sql_dialect is wired to "snowflake", the checker should infer the schema with
    // that real casing, so reading it back with the lowercase spelling actually
    // used in the query text is flagged as unknown-column (a real bug: it would be
    // a KeyError at runtime), while the correctly-cased access passes clean. This
    // is the exact-match-plus-folding behavior chosen over silently accepting both
    // cases.
    let source = r#"
import pandas as pd

df = pd.read_sql("SELECT order_id, amount FROM orders", conn)
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
    assert!(errors[0].message.contains("order_id"));
    assert!(
        errors[0].message.contains("ORDER_ID"),
        "expected a case-corrected suggestion, got: {}",
        errors[0].message
    );
}

#[test]
fn test_generic_dialect_does_not_fold_read_sql_columns() {
    // Without an explicit sql_dialect, columns keep the exact spelling from the
    // query text (SqlDialect::Generic preserves case) — the default, safest
    // behavior for engines the checker hasn't been told about.
    let source = r#"
import pandas as pd

df = pd.read_sql("SELECT order_id, amount FROM orders", conn)
print(df["order_id"])
"#;
    let mut linter = Linter::new();

    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    assert_eq!(errors.len(), 0, "errors: {errors:?}");
}

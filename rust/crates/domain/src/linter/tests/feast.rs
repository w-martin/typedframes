use super::*;

#[test]
fn test_feast_chained_form_with_unresolved_entity_df_is_unverifiable() {
    // entity_df is an undefined name here, so it never resolves -- the frame is
    // reported unverifiable once, on its first column use, no partial credit.
    let source = r#"
df = store.get_historical_features(
    entity_df=entity_df,
    features=["driver_stats:conv_rate", "driver_stats:acc_rate"],
).to_df()
print(df["conv_rate"])
print(df["driver_id"])
"#;
    let mut linter = Linter::new();
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors.iter().all(|e| e.code == CODE_UNVERIFIABLE_COLUMN));
    let schema_name = &linter.variables.get("df").unwrap().0;
    assert!(linter.unresolved_schemas.contains(schema_name));
}

#[test]
fn test_feast_with_concrete_entity_df_is_fully_typed() {
    // arrange: entity_df resolves to a concrete schema at the call site, so Feast's
    // real output (entity_df's own columns UNION the requested features) is fully
    // derivable -- no unresolved gap at all. A typo in the feature name IS caught.
    let source = r#"
import pandas as pd
entity_df = pd.read_csv("entities.csv", usecols=["driver_id", "event_timestamp"])
df = store.get_historical_features(
    entity_df=entity_df,
    features=["driver_stats:conv_rate"],
).to_df()
print(df["driver_id"])
print(df["conv_rate"])
print(df["cnv_rate"])
"#;
    let mut linter = Linter::new();
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
    assert!(errors[0].message.contains("cnv_rate"));
    assert_eq!(linter.dataframes_total, 2);
    assert_eq!(linter.dataframes_typed, 2);
    let schema_name = &linter.variables.get("df").unwrap().0;
    assert!(!linter.unresolved_schemas.contains(schema_name));
}

#[test]
fn test_should_not_count_a_feast_retrieval_with_unresolved_entity_df_as_typed() {
    // arrange: entity_df is never resolved, so the retrieval's real output can't be
    // fully derived -- it must NOT count as typed, even though the feature columns
    // alone did resolve.
    let source = r#"
df = store.get_historical_features(
    entity_df=entity_df,
    features=["driver_stats:conv_rate"],
).to_df()
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(linter.dataframes_total, 1);
    assert_eq!(linter.dataframes_typed, 0);
    assert_eq!(linter.untyped_sites.len(), 1);
    assert_eq!(linter.untyped_sites[0].var, "df");
}

#[test]
fn test_should_count_a_resolved_column_list_as_typed() {
    // arrange: a load with a real, exhaustive column list is typed AND concrete.
    let source = r#"
import pandas as pd
df = pd.read_csv("a.csv", usecols=["order_id", "amount"])
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(linter.dataframes_total, 1);
    assert_eq!(linter.dataframes_typed, 1);
    assert!(linter.untyped_sites.is_empty());
}

#[test]
fn test_should_not_count_a_bare_dataframe_returning_function_call_as_typed() {
    // arrange: the same-file counterpart of the external-package case -- a bare
    // `-> pd.DataFrame` return with no attached Schema resolves through
    // OPEN_FRAME_MARKER into an unresolved schema, so the call site is recognized
    // but does NOT count as typed.
    let source = r#"
import pandas as pd


def load_users() -> pd.DataFrame:
    ...


df = load_users()
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(linter.dataframes_total, 1);
    assert_eq!(linter.dataframes_typed, 0);
    assert_eq!(linter.untyped_sites.len(), 1);
    assert_eq!(linter.untyped_sites[0].var, "df");
}

#[test]
fn test_should_keep_a_merge_with_an_unresolved_input_from_counting_as_typed() {
    // arrange: merging an unresolved frame with a concrete one cannot recover the
    // missing columns, so the result stays unresolved too -- and does NOT count as
    // typed, even though it's still a recognized DataFrame origin.
    let source = r#"
import pandas as pd
feast_df = store.get_historical_features(
    entity_df=entity_df,
    features=["driver_stats:conv_rate"],
).to_df()
orders = pd.read_csv("orders.csv", usecols=["order_id"])
joined = feast_df.merge(orders)
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert: three origins recognized; only the plain read_csv is typed -- the
    // Feast frame and the merge result are both unresolved.
    assert_eq!(linter.dataframes_total, 3);
    assert_eq!(linter.dataframes_typed, 1);
    let untyped_vars: Vec<&str> = linter
        .untyped_sites
        .iter()
        .map(|s| s.var.as_str())
        .collect();
    assert_eq!(untyped_vars, vec!["feast_df", "joined"]);
}

#[test]
fn test_should_keep_untyped_sites_in_step_with_the_typed_count() {
    // arrange: the invariant the term-missing report relies on --
    // `untyped_sites.len() == total - typed`. A mix of every outcome in one file:
    // concrete, unresolved-via-bare-return, and unresolved-via-unparseable-load.
    let source = r#"
import pandas as pd


def load_users() -> pd.DataFrame:
    ...


concrete = pd.read_csv("a.csv", usecols=["a"])
open_frame = load_users()
unresolved = pd.read_csv("b.csv")
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(linter.dataframes_total, 3);
    assert_eq!(linter.dataframes_typed, 1);
    assert_eq!(
        linter.untyped_sites.len(),
        linter.dataframes_total - linter.dataframes_typed
    );
}

#[test]
fn test_feast_split_form_job_not_treated_as_dataframe_before_to_df() {
    // entity_df is unresolved here (an undefined name), so `df` is reported
    // unverifiable once it is materialized -- the point of this test is the `job`
    // assertion, which holds regardless.
    let source = r#"
job = store.get_historical_features(
    entity_df=entity_df,
    features=["driver_stats:conv_rate"],
)
df = job.to_df()
print(df["conv_rate"])
print(df["driver_id"])
"#;
    let mut linter = Linter::new();
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors.iter().all(|e| e.code == CODE_UNVERIFIABLE_COLUMN));
    assert!(
        !linter.variables.contains_key("job"),
        "job (a RetrievalJob, not a DataFrame) should not be tracked in self.variables"
    );
}

#[test]
fn test_feast_full_feature_names_uses_double_underscore() {
    // entity_df resolves concretely here, so the retrieval is fully typed and the
    // double-underscore feature name formatting is actually exercised by a real
    // unknown-column check, not just an absence of errors.
    let source = r#"
import pandas as pd
entity_df = pd.read_csv("entities.csv", usecols=["driver_id"])
df = store.get_historical_features(
    entity_df=entity_df,
    features=["driver_stats:conv_rate"],
    full_feature_names=True,
).to_df()
print(df["driver_stats__conv_rate"])
print(df["driver_stats__cnv_rate"])
"#;
    let mut linter = Linter::new();
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
    assert!(errors[0].message.contains("driver_stats__cnv_rate"));
}

#[test]
fn test_feast_unresolvable_features_falls_through_to_untracked_dataframe() {
    let source = r#"
feature_names = get_feature_list()
df = store.get_historical_features(
    entity_df=entity_df,
    features=feature_names,
).to_df()
"#;
    let mut linter = Linter::new();
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNTRACKED_DATAFRAME);
}

use super::*;

#[test]
fn test_should_resolve_divergent_call_sites_to_a_param_governed_feast_function_independently() {
    // arrange: load_conv_rate's `feature_names` parameter feeds a Feast
    // features=<param> call whose result is subscripted with "conv_rate" inside the
    // same function. Two call sites pass DIFFERENT literal feature lists for that
    // parameter -- the first resolves to {conv_rate} (the access is valid), the
    // second resolves to {acc_rate} (the access is NOT valid) -- and a third call
    // site passes a non-literal (dynamically built) list, which gets its OWN
    // untracked-dataframe diagnostic right there, rather than falling back to the
    // callee's generic one. This is the core case: the SAME callee line
    // (`print(df["conv_rate"])`) must NOT be flagged for the first call site while
    // being flagged for the second -- proven here by attributing the diagnostic to
    // the call site, not that line.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    let source = r#"
from feast import FeatureStore
import pandas as pd


def load_conv_rate(store: FeatureStore, entity_df: pd.DataFrame, feature_names: list) -> None:
    df = store.get_historical_features(entity_df=entity_df, features=feature_names).to_df()
    print(df["conv_rate"])


load_conv_rate(store, entity_df, ["driver_stats:conv_rate"])
load_conv_rate(store, entity_df, ["driver_stats:acc_rate"])
dynamic_names = build_feature_list()
load_conv_rate(store, entity_df, dynamic_names)
"#;
    let file_path = root.join("pipeline.py");
    fs::write(&file_path, source).unwrap();

    // act
    let index = build_index_internal(root);
    let file_path_str = file_path.to_str().unwrap().to_string();
    let mut linter = Linter::new();
    let mut errors = linter.check_file_internal(source, &file_path).unwrap();
    // Mirrors check_file's own retraction step: at least one call site here was
    // seen (all three were), so load_conv_rate is in resolved_governed and its
    // stale intra-function warning should be retracted.
    for (func_name, template) in &linter.param_governed_templates {
        if index
            .resolved_governed
            .contains(&(file_path_str.clone(), func_name.clone()))
        {
            errors.retain(|e| {
                !(e.line == template.governing_line
                    && e.col == template.governing_col
                    && e.code == CODE_UNTRACKED_DATAFRAME)
            });
        }
    }
    if let Some(extra) = index.call_site_errors.get(&file_path_str) {
        errors.extend(extra.iter().cloned());
    }
    errors.sort_by_key(|e| (e.line, e.col));

    // assert
    // The intra-function untracked-dataframe warning is suppressed: every call
    // site was seen, so "columns unknown at lint time" inside the callee is
    // stale/wrong, not a real signal. The first (valid literal) call site
    // produces nothing; the second (wrong literal) produces an unknown-column
    // error at ITS line; the third (non-literal) produces its OWN
    // untracked-dataframe diagnostic at ITS line, not the callee's.
    assert_eq!(errors.len(), 2, "errors: {errors:#?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
    assert_eq!(
        errors[0].line, 12,
        "should be attributed to the SECOND call site's line, not the callee's access line"
    );
    assert!(errors[0].message.contains("conv_rate"));
    assert_eq!(errors[1].code, CODE_UNTRACKED_DATAFRAME);
    assert_eq!(
        errors[1].line, 14,
        "should be attributed to the THIRD call site's line, not the callee's access line"
    );
    assert!(errors[1].message.contains("feature_names"));
}

#[test]
fn test_should_move_untracked_dataframe_warning_to_the_unresolvable_call_site() {
    // Regression test: a function with the exact same param-governed SHAPE as
    // load_conv_rate above, but where the only real call site passes a
    // dynamically built value, never a literal. The function itself is exactly as
    // resolvable as any other governed function -- the ambiguity originates at
    // the call site, not inside the callee -- so the diagnostic must move there:
    // resolved_governed DOES contain this pair (the call site was seen), the
    // callee's own generic line is retracted, and the call site gets its own
    // untracked-dataframe diagnostic instead. Silently dropping the diagnostic
    // entirely (going from "we tell you it's unknown" to "we tell you nothing at
    // all") would be a real regression -- the fix is relocation, not deletion.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    let source = r#"
from feast import FeatureStore
import pandas as pd


def load_conv_rate(store: FeatureStore, entity_df: pd.DataFrame, feature_names: list) -> None:
    df = store.get_historical_features(entity_df=entity_df, features=feature_names).to_df()
    print(df["conv_rate"])


dynamic_names = build_feature_list()
load_conv_rate(store, entity_df, dynamic_names)
"#;
    let file_path = root.join("pipeline.py");
    fs::write(&file_path, source).unwrap();

    // act
    let index = build_index_internal(root);
    let file_path_str = file_path.to_str().unwrap().to_string();
    assert!(
        index
            .resolved_governed
            .contains(&(file_path_str.clone(), "load_conv_rate".to_string())),
        "the call site was seen (even though its argument didn't resolve), so \
             load_conv_rate must be in resolved_governed"
    );

    let mut linter = Linter::new();
    let mut errors = linter.check_file_internal(source, &file_path).unwrap();
    for (func_name, template) in &linter.param_governed_templates {
        if index
            .resolved_governed
            .contains(&(file_path_str.clone(), func_name.clone()))
        {
            errors.retain(|e| {
                !(e.line == template.governing_line
                    && e.col == template.governing_col
                    && e.code == CODE_UNTRACKED_DATAFRAME)
            });
        }
    }
    if let Some(extra) = index.call_site_errors.get(&file_path_str) {
        errors.extend(extra.iter().cloned());
    }

    // assert: the callee's own line (7) is retracted; the diagnostic reappears at
    // the call site's line (12) instead.
    assert_eq!(errors.len(), 1, "errors: {errors:#?}");
    assert_eq!(errors[0].code, CODE_UNTRACKED_DATAFRAME);
    assert_eq!(
        errors[0].line, 12,
        "should be attributed to the call site, not the callee's access line"
    );
    assert!(errors[0].message.contains("feature_names"));
}

#[test]
fn test_unrelated_to_df_call_is_left_alone() {
    // `.to_df()` on something that never went through a recognized Feast
    // retrieval call must not be treated as a Feast result at all.
    let source = r#"
df = some_other_object.to_df()
"#;
    let mut linter = Linter::new();
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    assert_eq!(errors.len(), 0, "errors: {errors:?}");
    assert!(!linter.variables.contains_key("df"));
}

#[test]
fn test_bigquery_chained_query_to_dataframe_resolves_columns() {
    let source = r#"
df = client.query("SELECT user_id, total_spent FROM analytics.customers").to_dataframe()
print(df["user_id"])
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
fn test_pyspark_chained_sql_to_pandas_resolves_columns() {
    let source = r#"
df = spark.sql("SELECT order_id, amount FROM orders").toPandas()
print(df["order_id"])
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
fn test_duckdb_chained_sql_df_resolves_columns() {
    let source = r#"
df = duckdb.sql("SELECT order_id, amount FROM 'orders.parquet'").df()
print(df["order_id"])
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
fn test_unrelated_df_method_is_left_alone_not_treated_as_sql() {
    // `.df()` chained onto something that ISN'T one of SQL_PRODUCING_METHODS must
    // not be treated as a SQL-producing call at all — no dataframes_total bump, no
    // untracked-dataframe warning, since plenty of unrelated code has a `.df()`
    // accessor for something else entirely.
    let source = r#"
df = some_relation.transform().df()
"#;
    let mut linter = Linter::new();
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    assert_eq!(errors.len(), 0, "errors: {errors:?}");
    assert_eq!(linter.dataframes_total, 0);
}

#[test]
fn test_snowflake_cursor_execute_then_fetch_pandas_all_resolves_columns() {
    let source = r#"
cursor.execute("SELECT order_id, amount FROM orders")
df = cursor.fetch_pandas_all()
print(df["order_id"])
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
fn test_cursor_reexecute_with_unresolvable_query_poisons_previous_entry() {
    // A second execute() with an unresolvable (variable) query must not leave the
    // first execute()'s columns silently in effect for the next fetch.
    let source = r#"
cursor.execute("SELECT order_id FROM orders")
cursor.execute(dynamic_query)
df = cursor.fetch_pandas_all()
"#;
    let mut linter = Linter::new();
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNTRACKED_DATAFRAME);
}

#[test]
fn test_connectorx_read_sql_uses_second_positional_argument() {
    let source = r#"
import connectorx as cx

df = cx.read_sql(conn_uri, "SELECT order_id, amount FROM orders")
print(df["order_id"])
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
fn test_visitor_various_stmts() {
    let source = r#"
class Other: pass
def func(): pass
x = 1
"#;
    let mut linter = Linter::new();
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();
    assert!(errors.is_empty());
}

#[test]
fn test_no_false_positive_on_method_call_name() {
    // `df.assign(created_at="2024-01-01")` must NOT raise
    // "Column 'assign' does not exist" — method names are not column accesses.
    let source = r#"
from typedframes import BaseSchema, Column

class UserData(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)

import pandas as pd

df: DataFrame[UserData] = pd.read_csv("users.csv")
augmented = df.assign(created_at="2024-01-01")
print(augmented["user_id"])
"#;
    let mut linter = Linter::new();
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
}

use super::*;

#[test]
fn test_should_resolve_param_governed_call_site_across_files() {
    // arrange: load_conv_rate is defined in helpers.py; pipeline.py calls it twice
    // with different literals, same as the same-file test above, but now resolved
    // through resolve_delegate_target's cross-file import resolution.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    fs::write(
        root.join("helpers.py"),
        r#"
from feast import FeatureStore
import pandas as pd


def load_conv_rate(store: FeatureStore, entity_df: pd.DataFrame, feature_names: list) -> None:
    df = store.get_historical_features(entity_df=entity_df, features=feature_names).to_df()
    print(df["conv_rate"])
"#,
    )
    .unwrap();
    let pipeline_source = r#"
from helpers import load_conv_rate

load_conv_rate(store, entity_df, ["driver_stats:conv_rate"])
load_conv_rate(store, entity_df, ["driver_stats:acc_rate"])
"#;
    let pipeline_path = root.join("pipeline.py");
    fs::write(&pipeline_path, pipeline_source).unwrap();

    // act
    let index = build_index_internal(root);
    let pipeline_path_str = pipeline_path.to_str().unwrap().to_string();

    // assert
    let errors = index
        .call_site_errors
        .get(&pipeline_path_str)
        .cloned()
        .unwrap_or_default();
    assert_eq!(errors.len(), 1, "errors: {errors:#?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
    assert_eq!(errors[0].line, 5, "should be the SECOND call site only");
    assert!(errors[0].message.contains("conv_rate"));
    assert!(errors[0].message.contains("helpers.py"));
}

#[test]
fn test_should_resolve_call_site_argument_through_a_multi_hop_delegate_chain() {
    // A call site doesn't have to pass the literal directly -- it can pass a call
    // to a zero-arg helper, which itself just forwards to ANOTHER helper, which
    // finally returns the literal. resolve_returns_literal_list must follow that
    // whole chain (helper_a -> helper_b -> literal), not just one hop.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    let source = r#"
from feast import FeatureStore
import pandas as pd


def load_conv_rate(store: FeatureStore, entity_df: pd.DataFrame, feature_names: list) -> None:
    df = store.get_historical_features(entity_df=entity_df, features=feature_names).to_df()
    print(df["conv_rate"])


def helper_b() -> list[str]:
    return ["driver_stats:acc_rate"]


def helper_a() -> list[str]:
    return helper_b()


load_conv_rate(store, entity_df, helper_a())
"#;
    let file_path = root.join("pipeline.py");
    fs::write(&file_path, source).unwrap();

    // act
    let index = build_index_internal(root);

    // assert: helper_a resolves (through helper_b) to {acc_rate}, which does NOT
    // satisfy load_conv_rate's print(df["conv_rate"]) -- caught two hops away.
    let file_path_str = file_path.to_str().unwrap().to_string();
    let errors = index
        .call_site_errors
        .get(&file_path_str)
        .cloned()
        .unwrap_or_default();
    assert_eq!(errors.len(), 1, "errors: {errors:#?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
    assert!(errors[0].message.contains("conv_rate"));
    assert!(
        index
            .resolved_governed
            .contains(&(file_path_str, "load_conv_rate".to_string())),
        "load_conv_rate should be resolved_governed via the multi-hop chain"
    );
}

#[test]
fn test_should_resolve_call_site_argument_through_literal_substitution_into_an_fstring() {
    // A call site can pass a call like `feature_names("driver_stats")` -- a
    // genuine argument, not a zero-arg forward -- whose callee builds its return
    // value with an f-string that interpolates that very parameter. The evaluator
    // must substitute the literal "driver_stats" for the callee's own parameter
    // and evaluate the f-string with it, arriving at the same resolved feature list
    // as if the call site had written the literal out directly.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    let source = r#"
from feast import FeatureStore
import pandas as pd


def load_conv_rate(store: FeatureStore, entity_df: pd.DataFrame, feature_names: list) -> None:
    df = store.get_historical_features(entity_df=entity_df, features=feature_names).to_df()
    print(df["conv_rate"])


def feature_names(prefix: str) -> list[str]:
    return [f"{prefix}:conv_rate"]


load_conv_rate(store, entity_df, feature_names("driver_stats"))
"#;
    let file_path = root.join("pipeline.py");
    fs::write(&file_path, source).unwrap();

    // act
    let index = build_index_internal(root);

    // assert: feature_names("driver_stats") resolves (via argument substitution
    // into the f-string) to {"driver_stats:conv_rate"}, which DOES satisfy
    // load_conv_rate's print(df["conv_rate"]) -- no error, and the callee's own
    // untracked-dataframe warning is retracted.
    let file_path_str = file_path.to_str().unwrap().to_string();
    let errors = index
        .call_site_errors
        .get(&file_path_str)
        .cloned()
        .unwrap_or_default();
    assert!(errors.is_empty(), "errors: {errors:#?}");
    assert!(
        index
            .resolved_governed
            .contains(&(file_path_str, "load_conv_rate".to_string())),
        "load_conv_rate should be resolved_governed via argument substitution"
    );
}

#[test]
fn test_should_not_hang_on_a_self_delegating_return_chain() {
    // Recursion protection: a function whose `return` shape delegates to itself
    // (or a cycle of functions delegating to each other) must resolve to
    // unresolvable rather than looping forever -- mirrors
    // test_should_not_hang_on_mutually_delegating_functions's existing coverage
    // for the *requires* side of this same cycle-protection pattern.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    let source = r#"
from feast import FeatureStore
import pandas as pd


def load_conv_rate(store: FeatureStore, entity_df: pd.DataFrame, feature_names: list) -> None:
    df = store.get_historical_features(entity_df=entity_df, features=feature_names).to_df()
    print(df["conv_rate"])


def cyclic_a() -> list[str]:
    return cyclic_b()


def cyclic_b() -> list[str]:
    return cyclic_a()


load_conv_rate(store, entity_df, cyclic_a())
"#;
    let file_path = root.join("pipeline.py");
    fs::write(&file_path, source).unwrap();

    // act -- must terminate (this test itself hanging is the failure mode)
    let index = build_index_internal(root);

    // assert: the argument is unresolvable (the cycle guard gives up), but the
    // call site itself was still seen -- so its own untracked-dataframe
    // diagnostic is reported right there, and load_conv_rate is in
    // resolved_governed so the callee's own generic line is retracted.
    let file_path_str = file_path.to_str().unwrap().to_string();
    let errors = index
        .call_site_errors
        .get(&file_path_str)
        .cloned()
        .unwrap_or_default();
    assert_eq!(errors.len(), 1, "errors: {errors:#?}");
    assert_eq!(errors[0].code, CODE_UNTRACKED_DATAFRAME);
    assert!(index
        .resolved_governed
        .contains(&(file_path_str, "load_conv_rate".to_string())));
}

#[test]
fn test_should_resolve_transitive_requires_through_plain_import_delegate() {
    // arrange: transform() only forwards df to steps.postproc(df) via a plain
    // `import steps` — the delegate-detection blind spot for attribute calls.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    fs::write(
        root.join("steps.py"),
        r#"
import pandas as pd

def postproc(df: pd.DataFrame) -> pd.DataFrame:
    y = df["c"]
    return df
"#,
    )
    .unwrap();
    fs::write(
        root.join("transforms.py"),
        r#"
import steps

def transform(df):
    return steps.postproc(df)
"#,
    )
    .unwrap();

    // act
    let index = build_index_internal(root);
    let transforms_path = root.join("transforms.py").to_str().unwrap().to_string();
    let func = &index.files[&transforms_path].functions["transform"];

    // assert: transform's own requires is the transitive union through the
    // attribute-style delegate call, i.e. {c} — not empty, which is what it
    // would be if the `steps.postproc(df)` call were never recognised as a
    // delegate at all.
    assert_eq!(func.requires, vec!["c".to_string()]);
}

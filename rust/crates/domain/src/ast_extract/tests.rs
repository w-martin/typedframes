use super::*;
use ruff_python_parser::parse_module;

fn annotation_of(source: &str) -> ruff_python_ast::Expr {
    let parsed = parse_module(source).unwrap();
    let Stmt::FunctionDef(func) = &parsed.into_syntax().body[0] else {
        panic!("Expected FunctionDef");
    };
    (*func.returns.clone().expect("expected a return annotation")).clone()
}

fn dict_of(source: &str) -> ast::ExprDict {
    let parsed = parse_module(source).unwrap();
    let Stmt::Expr(expr_stmt) = &parsed.into_syntax().body[0] else {
        panic!("Expected an expression statement");
    };
    let Expr::Dict(dict) = (*expr_stmt.value).clone() else {
        panic!("Expected a dict literal");
    };
    dict
}

fn filter_call_of(source: &str) -> ast::ExprCall {
    let parsed = parse_module(source).unwrap();
    let Stmt::Expr(expr_stmt) = &parsed.into_syntax().body[0] else {
        panic!("Expected an expression statement");
    };
    let Expr::Call(call) = (*expr_stmt.value).clone() else {
        panic!("Expected a call expression");
    };
    call
}

#[test]
fn test_should_detect_base_schema_class() {
    // arrange/act/assert
    assert!(is_schema_base("BaseSchema"));
    assert!(is_schema_base("DataFrameModel"));
    assert!(is_schema_base("DataFrame"));
    assert!(is_schema_base("BaseFrame"));
    assert!(!is_schema_base("SomeOtherClass"));
}

#[test]
fn test_extract_schema_from_annotation() {
    let source = "x: DataFrame[MySchema] = df";
    let parsed = parse_module(source).unwrap();
    let stmt = &parsed.into_syntax().body[0];
    if let Stmt::AnnAssign(ann) = stmt {
        let schema = extract_schema_from_annotation(&ann.annotation);
        assert_eq!(schema, Some("MySchema"));
    } else {
        panic!("Expected AnnAssign");
    }
}

#[test]
fn test_should_detect_bare_pandas_and_polars_dataframe_return_types() {
    assert!(extract_bare_dataframe_type(&annotation_of(
        "def f() -> pd.DataFrame: ..."
    )));
    assert!(extract_bare_dataframe_type(&annotation_of(
        "def f() -> pl.DataFrame: ..."
    )));
    // Bare-imported form: `from pandas import DataFrame`, `-> DataFrame`.
    assert!(extract_bare_dataframe_type(&annotation_of(
        "def f() -> DataFrame: ..."
    )));
}

#[test]
fn test_should_not_detect_a_schema_subscripted_dataframe_as_bare() {
    // DataFrame[Schema] / Annotated[pd.DataFrame, Schema] have a real schema to
    // extract via extract_schema_from_annotation -- callers try that FIRST, so
    // extract_bare_dataframe_type only needs to reject them, not handle them.
    assert!(!extract_bare_dataframe_type(&annotation_of(
        "def f() -> DataFrame[MySchema]: ..."
    )));
    assert!(!extract_bare_dataframe_type(&annotation_of(
        "def f() -> Annotated[pd.DataFrame, MySchema]: ..."
    )));
}

#[test]
fn test_should_not_detect_an_unrelated_return_type_as_a_bare_dataframe() {
    assert!(!extract_bare_dataframe_type(&annotation_of(
        "def f() -> int: ..."
    )));
    assert!(!extract_bare_dataframe_type(&annotation_of(
        "def f() -> pd.Series: ..."
    )));
    assert!(!extract_bare_dataframe_type(&annotation_of(
        "def f() -> None: ..."
    )));
}

#[test]
fn test_should_extract_the_keys_of_a_dict_literal_in_order_without_duplicates() {
    let dict = dict_of("{\"b\": 1, \"a\": 2, \"b\": 3}");
    assert_eq!(
        extract_dict_literal_keys(&dict),
        Some(vec!["b".to_string(), "a".to_string()])
    );
}

#[test]
fn test_should_not_extract_keys_from_an_empty_dict_a_spread_or_a_computed_key() {
    assert_eq!(extract_dict_literal_keys(&dict_of("{}")), None);
    assert_eq!(
        extract_dict_literal_keys(&dict_of("{\"a\": 1, **extra}")),
        None
    );
    assert_eq!(
        extract_dict_literal_keys(&dict_of("{\"a\": 1, k: 2}")),
        None
    );
    assert_eq!(
        extract_dict_literal_keys(&dict_of("{\"a\": 1, 2: 2}")),
        None
    );
}

#[test]
fn test_should_treat_filter_items_as_column_selecting_when_axis_is_absent() {
    assert!(filter_is_column_selecting(&filter_call_of(
        "df.filter(items=[\"a\"])"
    )));
}

#[test]
fn test_should_treat_filter_items_as_column_selecting_when_axis_is_columns() {
    assert!(filter_is_column_selecting(&filter_call_of(
        "df.filter(items=[\"a\"], axis=1)"
    )));
    assert!(filter_is_column_selecting(&filter_call_of(
        "df.filter(items=[\"a\"], axis=\"columns\")"
    )));
}

#[test]
fn test_should_not_treat_filter_items_as_column_selecting_when_axis_is_index() {
    // arrange/act/assert: axis=0 / axis="index" redirects items=/like=/regex= to
    // INDEX LABELS, not columns -- this checker has no model for that, so it
    // must be left alone entirely, the same as the row-filtering
    // `.filter(mask)` form.
    assert!(!filter_is_column_selecting(&filter_call_of(
        "df.filter(items=[\"a\"], axis=0)"
    )));
    assert!(!filter_is_column_selecting(&filter_call_of(
        "df.filter(items=[\"a\"], axis=\"index\")"
    )));
}

#[test]
fn test_should_not_treat_a_plain_positional_filter_as_column_selecting() {
    assert!(!filter_is_column_selecting(&filter_call_of(
        "df.filter(mask)"
    )));
}

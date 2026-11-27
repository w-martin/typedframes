use super::*;

#[test]
fn test_should_count_typed_dataframe_for_load_call_inside_if_block() {
    // arrange: the same load call, but reached only through an `if` branch --
    // no nesting beyond that. Not a "deeply nested control flow" edge case.
    //
    // A call site (`result = load(True)`) is required here: a bare pass-through
    // return moves its origin's count to each call site that resolves through it
    // (see the journey/leg model -- one DataFrame per execution-path instance,
    // not one per internal assignment plus one per call), so a `load` that is
    // never actually called would count zero, which is NOT what this test is
    // about -- it exists to verify the `if`-nested load call is still recognized
    // once reached through a real call site.
    let source = r#"
import pandas as pd

def load(flag):
    if flag:
        df = pd.read_csv("data.csv", usecols=["a", "b"])
        print(df["a"])
        return df
    return None

result = load(True)
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
fn test_should_count_typed_dataframe_for_load_call_inside_if_else_branch() {
    // arrange: the load lives in the `else` arm (an ElifElseClause), not the `if`
    // arm -- a distinct AST path from the test above. A call site is required --
    // see test_should_count_typed_dataframe_for_load_call_inside_if_block's
    // comment on why.
    let source = r#"
import pandas as pd

def load(flag):
    if flag:
        return None
    else:
        df = pd.read_csv("data.csv", usecols=["a", "b"])
        print(df["a"])
        return df

result = load(False)
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
fn test_should_count_typed_dataframe_for_load_call_inside_with_block() {
    // arrange: the pattern a real SQL connection almost always uses -- a
    // context-managed connection/cursor wrapping the actual read call. A call
    // site is required -- see
    // test_should_count_typed_dataframe_for_load_call_inside_if_block's comment
    // on why.
    let source = r#"
import pandas as pd

def load(engine):
    with engine.connect() as conn:
        df = pd.read_sql("SELECT order_id, amount FROM orders", conn)
        print(df["order_id"])
        return df

result = load(some_engine)
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
fn test_should_count_typed_dataframe_for_load_call_inside_try_and_except() {
    // arrange: a load call in the `try` body, and a second in the `except`
    // fallback -- both bodies must be visited. A call site is required -- see
    // test_should_count_typed_dataframe_for_load_call_inside_if_block's comment
    // on why.
    let source = r#"
import pandas as pd

def load(path):
    try:
        df = pd.read_csv(path, usecols=["a", "b"])
    except FileNotFoundError:
        df = pd.read_csv("fallback.csv", usecols=["a", "b"])
    return df

result = load("orders.csv")
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert: two independent origins -- the `try` branch's own (never reassigned
    // before `return`, so its count stays right where it happened) and the
    // `except` branch's, whose count moves to the call site (`result = load(...)`)
    // since -- being the LAST binding of `df` before the bare `return df` -- it's
    // the one a caller actually observes.
    assert_eq!(linter.dataframes_total, 2);
    assert_eq!(linter.dataframes_typed, 2);
}

#[test]
fn test_should_count_typed_dataframe_for_load_call_inside_for_loop() {
    // arrange
    let source = r#"
import pandas as pd

def load(paths):
    for path in paths:
        df = pd.read_csv(path, usecols=["a", "b"])
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
fn test_should_count_typed_dataframe_for_load_call_inside_while_loop() {
    // arrange
    let source = r#"
import pandas as pd

def load(has_more):
    while has_more():
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
fn test_should_count_untyped_dataframe_for_bare_load_call() {
    // arrange: no usecols/columns, so the checker has no column information
    let source = r#"
import pandas as pd

df = pd.read_csv("data.csv")
print(df["a"])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert: counted as seen, but not typed — and still raises the existing warning
    assert_eq!(linter.dataframes_total, 1);
    assert_eq!(linter.dataframes_typed, 0);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].code, "untracked-dataframe");
}

#[test]
fn test_should_record_unresolved_call_for_attribute_call_used_like_a_dataframe() {
    // arrange: `internal_db.run_query` isn't any recognized load pattern, but `df`
    // is subscripted afterward exactly the way a DataFrame would be. Must be
    // recorded silently -- no error, no dataframes_total/untyped_sites change.
    let source = r#"
import internal_db

df = internal_db.run_query("orders.sql", conn)
print(df["order_id"])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(linter.dataframes_total, 0);
    assert!(linter.untyped_sites.is_empty());
    assert!(errors.is_empty());
    assert_eq!(linter.unresolved_dataframe_shaped_calls.len(), 1);
    assert_eq!(
        linter.unresolved_dataframe_shaped_calls[0].name,
        "internal_db"
    );
    assert!(linter.unresolved_dataframe_shaped_calls[0].is_attribute_call);
}

#[test]
fn test_should_record_unresolved_call_for_bare_call_used_like_a_dataframe() {
    // arrange: `from internal_db import run_query; df = run_query(...)` -- same
    // scenario, but the callee is a bare name, not an attribute access.
    let source = r#"
from internal_db import run_query

df = run_query("orders.sql", conn)
print(df["order_id"])
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(linter.unresolved_dataframe_shaped_calls.len(), 1);
    assert_eq!(
        linter.unresolved_dataframe_shaped_calls[0].name,
        "run_query"
    );
    assert!(!linter.unresolved_dataframe_shaped_calls[0].is_attribute_call);
}

#[test]
fn test_should_not_record_unresolved_call_never_used_like_a_dataframe() {
    // arrange: same unrecognized call, but the result is never subscripted or
    // DataFrame-method-called -- e.g. a logger. Must not be recorded just because
    // the function name isn't on any allowlist.
    let source = r#"
import logging

logger = logging.getLogger("app")
logger.debug("hello")
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert!(linter.unresolved_dataframe_shaped_calls.is_empty());
}

#[test]
fn test_should_not_record_unresolved_call_for_recognized_pandas_and_polars_loads() {
    // arrange: normal, fully-recognized pd/pl usage -- every one of these is
    // resolved directly by the existing load-call dispatch, so none should ever
    // reach the unresolved-call fallback. Guards against pandas/polars themselves
    // ever being swept into external-package candidate discovery: LOAD_MODULES
    // calls always bump dataframes_total, so they short-circuit before the
    // unresolved-call check even runs.
    let source = r#"
import pandas as pd
import polars as pl

df = pd.read_csv("data.csv", usecols=["a", "b"])
print(df["a"])
lazy = pl.read_csv("other.csv", columns=["x", "y"])
print(lazy["x"])
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert!(linter.unresolved_dataframe_shaped_calls.is_empty());
}

#[test]
fn test_should_record_untyped_site_with_variable_name_and_position() {
    // arrange
    let source = r#"
import pandas as pd

sales = pd.read_csv("data.csv")
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert: the site names the variable that cost coverage, so the CLI can
    // point at the assignment rather than only reporting a ratio
    assert_eq!(linter.untyped_sites.len(), 1);
    assert_eq!(linter.untyped_sites[0].var, "sales");
    assert_eq!(linter.untyped_sites[0].line, 4);
}

#[test]
fn test_should_record_no_untyped_sites_when_every_dataframe_resolves() {
    // arrange
    let source = r#"
import pandas as pd

df = pd.read_csv("data.csv", usecols=["a", "b"])
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert!(linter.untyped_sites.is_empty());
}

#[test]
fn test_should_name_suggested_and_withheld_sites_with_their_phrases() {
    // arrange
    let source = r#"
import pandas as pd


def good():
    df = pd.read_json("x.json")
    return df["a"]


def blocked():
    raw = pd.read_json("y.json")
    print(raw["a"])
    return clean(raw)
"#;

    // act
    let linter = suggest_for(source);

    // assert
    assert_eq!(linter.suggestions.len(), 1);
    assert_eq!(linter.withheld.len(), 1);
    let phrase_of = |var: &str| {
        linter
            .untyped_sites
            .iter()
            .find(|s| s.var == var)
            .map(|s| s.phrase.clone())
            .unwrap()
    };
    assert_eq!(linter.suggestions[0].phrase, phrase_of("df"));
    assert_eq!(linter.withheld[0].phrase, phrase_of("raw"));
    assert_ne!(phrase_of("df"), phrase_of("raw"));
    assert!(!phrase_of("df").is_empty());
}

#[test]
fn test_should_keep_a_sites_phrase_when_unrelated_code_is_added_above_it() {
    // arrange
    let load = "    df = pd.read_json(\"x.json\")\n    return df[\"a\"]\n";
    let before = format!("import pandas as pd\n\n\ndef load():\n{load}");
    let after = format!(
        "import pandas as pd\n\nLIMIT = 5\n\n\ndef other():\n    return 1\n\n\ndef load():\n{load}"
    );

    // act
    let phrase = |source: &str| suggest_for(source).untyped_sites[0].phrase.clone();

    // assert
    assert_eq!(phrase(&before), phrase(&after));
}

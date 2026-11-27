use super::*;

#[test]
fn test_should_infer_columns_from_dataframe_constructor_dict_literal() {
    // arrange: a dict literal passed directly to DataFrame() names its columns as
    // its own keys, with no columns=/usecols= kwarg needed -- the checker should
    // infer {a, b} from the literal itself, precisely enough to catch a typo.
    let source = r#"
import pandas as pd

df = pd.DataFrame({"a": [1, 2], "b": [3, 4]})
print(df["a"])
print(df["c"])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(linter.dataframes_total, 1);
    assert_eq!(linter.dataframes_typed, 1);
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
    assert!(errors[0].message.contains("c"));
}

#[test]
fn test_should_count_untracked_dataframe_for_bare_constructor_call() {
    // arrange: no columns=, no dict literal -- the checker has no static column
    // information, so this should fall to the same untracked-dataframe treatment
    // a bare read_csv(path) gets (recognized but untyped), not go unrecognized
    // entirely.
    let source = r#"
import pandas as pd

df = pd.DataFrame(records)
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(linter.dataframes_total, 1);
    assert_eq!(linter.dataframes_typed, 0);
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNTRACKED_DATAFRAME);
    // The constructor takes no usecols= kwarg -- the hint must not suggest one.
    assert!(!errors[0].message.contains("usecols"));
    assert!(errors[0].message.contains("columns="));
}

#[test]
fn test_should_count_typed_dataframe_for_polars_constructor_with_schema_dict() {
    // arrange: schema={...} is polars' dtype=-shaped kwarg on its own DataFrame()
    // constructor -- same dict-keys extraction LOAD_FUNCTIONS already gives dtype=.
    let source = r#"
import polars as pl

df = pl.DataFrame(data, schema={"a": int, "b": str})
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
fn test_should_record_typed_site_location_for_a_recognized_load_call() {
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
    assert_eq!(linter.typed_sites.len(), 1);
    assert_eq!(linter.typed_sites[0].line, 4);
    assert_eq!(linter.typed_sites[0].var, "df");
    assert!(linter.untyped_sites.is_empty());
}

#[test]
fn test_should_scan_dataframe_shaped_calls_regardless_of_syntactic_position() {
    // arrange: the returned load counts like the two-line form (see
    // `returned_loads`); the `.append(...)` argument and the call argument are
    // not the direct RHS of a plain `x = ...` assignment, so the targeted
    // counting logic recognizes neither. The position-independent scan must
    // still find all three, each labelled with where it was actually found.
    let source = r#"
import pandas as pd


def load(path):
    return pd.read_csv(path, usecols=["a", "b"])


def build():
    frames = []
    frames.append(pd.read_csv("x.csv", usecols=["a"]))
    train(pd.DataFrame({"a": [1]}, columns=["a", "b"]))
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert: only the returned load is counted.
    assert_eq!(linter.dataframes_total, 1);
    assert_eq!(linter.typed_sites.len(), 1);
    assert!(linter.untyped_sites.is_empty());

    // assert: the scan finds all three anyway, correctly labelled.
    assert_eq!(linter.all_dataframe_calls.len(), 3);
    let labels: Vec<&str> = linter
        .all_dataframe_calls
        .iter()
        .map(|s| s.label.as_str())
        .collect();
    assert_eq!(labels, vec!["pd.read_csv", "pd.read_csv", "pd.DataFrame"]);
    assert!(linter.all_dataframe_calls[0]
        .context
        .contains("plain `x = ...`"));
    assert!(linter.all_dataframe_calls[1]
        .context
        .contains("call argument"));
    assert!(linter.all_dataframe_calls[2]
        .context
        .contains("call argument"));
}

#[test]
fn test_should_label_collection_element_and_direct_assignment_contexts() {
    // arrange
    let source = r#"
import pandas as pd

frames = [pd.read_csv("a.csv", usecols=["a"]), pd.read_csv("b.csv", usecols=["b"])]
df = pd.read_csv("c.csv", usecols=["c"])
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert: only the direct `df = ...` assignment is actually counted.
    assert_eq!(linter.dataframes_total, 1);
    assert_eq!(linter.typed_sites.len(), 1);

    // assert: the scan finds all three, with the list elements and the direct
    // assignment labelled distinctly, and the counted one's line matches the
    // scan's own record of it -- the line-based cross-reference the CLI's
    // `--coverage-detail=explain` relies on.
    assert_eq!(linter.all_dataframe_calls.len(), 3);
    assert!(linter.all_dataframe_calls[0].context.contains("element"));
    assert!(linter.all_dataframe_calls[1].context.contains("element"));
    assert!(linter.all_dataframe_calls[2]
        .context
        .contains("direct value"));
    assert_eq!(
        linter.all_dataframe_calls[2].line,
        linter.typed_sites[0].line
    );
}

#[test]
fn test_should_count_typed_dataframe_for_load_call_inside_class_method() {
    // arrange: the exact same load call as
    // test_should_count_typed_dataframe_for_load_call_with_usecols, but written
    // inside an ordinary (non-BaseSchema, non-ORM) class's method body rather than
    // at module level. `Loader` doesn't inherit from BaseSchema and has no
    // `__tablename__`, so nothing marks it as schema-bearing -- it's just a plain
    // class, the shape most OO/DI-style codebases actually use to group loaders.
    //
    // A call site is required -- see
    // test_should_count_typed_dataframe_for_load_call_inside_if_block's comment on
    // why -- reached here through self.<attr>, the proven-working resolution path
    // (see test_should_resolve_self_attr_method_with_no_return_annotation_via_body_inference).
    let source = r#"
import pandas as pd

class Loader:
    def load(self):
        df = pd.read_csv("data.csv", usecols=["a", "b"])
        print(df["a"])
        return df


class Runner:
    def __init__(self):
        self._loader = Loader()

    def run(self):
        result = self._loader.load()
        print(result["a"])
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert: the load call is inside a class method body, but must be seen just
    // as if it were at module level or inside a free function
    assert_eq!(linter.dataframes_total, 1);
    assert_eq!(linter.dataframes_typed, 1);
}

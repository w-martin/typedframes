use super::*;

#[test]
fn test_should_observe_downstream_usage_for_a_module_level_untyped_site() {
    // arrange
    let source = r#"
import pandas as pd

sales = pd.read_csv("data.csv")
print(sales["a"])
if flag:
    print(sales["b"])
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(linter.untyped_sites.len(), 1);
    let site = &linter.untyped_sites[0];
    let usage = linter
        .untyped_site_usage
        .get(&(site.line, site.col))
        .expect("usage recorded for the module-level site");
    assert_eq!(
        usage.accesses,
        vec![
            column_usage::ColumnAccess {
                column: "a".to_string(),
                conditional: false,
                line: 5,
                via: None,
            },
            column_usage::ColumnAccess {
                column: "b".to_string(),
                conditional: true,
                line: 7,
                via: None,
            },
        ]
    );
}

#[test]
fn test_should_observe_downstream_usage_for_a_function_level_untyped_site() {
    // arrange
    let source = r#"
import pandas as pd

def run():
    sales = pd.read_csv("data.csv")
    print(sales["a"])
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    let site = &linter.untyped_sites[0];
    let usage = linter
        .untyped_site_usage
        .get(&(site.line, site.col))
        .unwrap();
    assert_eq!(usage.accesses[0].column, "a");
}

#[test]
fn test_should_observe_usage_for_an_origin_nested_inside_a_branch() {
    let source = r#"
import pandas as pd

if flag:
    sales = pd.read_csv("data.csv")
    print(sales["a"])
"#;
    let mut linter = Linter::new();

    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    assert_eq!(linter.untyped_sites.len(), 1);
    assert_eq!(linter.untyped_site_usage.len(), 1);
}

#[test]
fn test_should_suggest_for_an_origin_inside_a_method() {
    let source = r#"
import pandas as pd


class Report:
    def build(self):
        df = pd.read_json("x.json")
        return df["a"] + df["b"]
"#;
    let linter = suggest_for(source);

    assert!(linter.withheld.is_empty(), "{:?}", linter.withheld);
    assert_eq!(linter.suggestions.len(), 1);
    assert_eq!(linter.suggestions[0].tier, "safe");
    assert!(linter.suggestions[0]
        .diff
        .contains("+        df = df[[\"a\", \"b\"]]"));
}

#[test]
fn test_should_suggest_for_an_origin_inside_a_branch_and_count_reads_after_it() {
    let source = r#"
import pandas as pd


def run(flag):
    if flag:
        df = pd.read_json("x.json")
        print(df["a"])
    print(df["b"])
"#;
    let linter = suggest_for(source);

    assert_eq!(linter.suggestions.len(), 1, "{:?}", linter.withheld);
    assert_eq!(linter.suggestions[0].columns, vec!["a", "b"]);
    assert!(linter.suggestions[0]
        .diff
        .contains("+        df = df[[\"a\", \"b\"]]"));
}

#[test]
fn test_should_not_count_a_read_in_a_sibling_branch() {
    let source = r#"
import pandas as pd


def run(flag):
    if flag:
        df = pd.read_csv("x.csv")
        print(df["a"])
    else:
        df = pd.read_csv("y.csv")
        print(df["zz"])
"#;
    let linter = suggest_for(source);

    let columns: Vec<_> = linter
        .suggestions
        .iter()
        .map(|s| s.columns.clone())
        .collect();
    assert_eq!(columns, vec![vec!["a"], vec!["zz"]]);
}

#[test]
fn test_should_count_a_read_in_an_except_handler_after_an_origin_in_the_try_body() {
    let source = r#"
import pandas as pd


def run():
    try:
        df = pd.read_csv("x.csv")
        print(df["a"])
    except ValueError:
        print(df["b"])
    finally:
        print(df["c"])
"#;
    let linter = suggest_for(source);

    assert_eq!(linter.suggestions.len(), 1, "{:?}", linter.withheld);
    assert_eq!(linter.suggestions[0].columns, vec!["a", "b", "c"]);
}

#[test]
fn test_should_suggest_for_an_origin_inside_a_with_block() {
    let source = r#"
import pandas as pd


def run(lock):
    with lock:
        df = pd.read_json("x.json")
    print(df["a"])
"#;
    let linter = suggest_for(source);

    assert_eq!(linter.suggestions.len(), 1, "{:?}", linter.withheld);
    assert!(linter.suggestions[0]
        .diff
        .contains("+        df = df[[\"a\"]]"));
}

#[test]
fn test_should_withhold_an_origin_inside_a_loop() {
    for header in ["for i in items:", "while go():"] {
        let source = format!(
                "import pandas as pd\n\n\ndef run(items):\n    {header}\n        df = pd.read_csv(\"x.csv\")\n        print(df[\"a\"])\n"
            );
        let linter = suggest_for(&source);

        assert!(linter.suggestions.is_empty(), "{header}");
        assert!(
            linter.withheld[0].reason.contains("inside a loop"),
            "{header}"
        );
    }
}

#[test]
fn test_should_withhold_an_origin_in_a_branch_inside_a_loop() {
    let source = r#"
import pandas as pd


def run(items):
    for i in items:
        if i:
            df = pd.read_csv("x.csv")
            print(df["a"])
"#;
    let linter = suggest_for(source);

    assert!(linter.suggestions.is_empty());
    assert!(linter.withheld[0].reason.contains("inside a loop"));
}

#[test]
fn test_should_record_separate_usage_per_untyped_site() {
    // arrange
    let source = r#"
import pandas as pd

a = pd.read_csv("a.csv")
print(a["x"])
b = pd.read_csv("b.csv")
print(b["y"])
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(linter.untyped_sites.len(), 2);
    assert_eq!(linter.untyped_site_usage.len(), 2);
    let usage_a =
        &linter.untyped_site_usage[&(linter.untyped_sites[0].line, linter.untyped_sites[0].col)];
    let usage_b =
        &linter.untyped_site_usage[&(linter.untyped_sites[1].line, linter.untyped_sites[1].col)];
    assert_eq!(usage_a.accesses[0].column, "x");
    assert_eq!(usage_b.accesses[0].column, "y");
}

#[test]
fn test_should_omit_usage_when_nothing_downstream_was_observed() {
    // arrange: the site is recorded, but nothing ever reads from it.
    let source = r#"
import pandas as pd

sales = pd.read_csv("data.csv")
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert: no entry at all, rather than one holding two empty vecs.
    assert!(linter.untyped_site_usage.is_empty());
}

#[test]
fn test_should_keep_untyped_site_count_equal_to_total_minus_typed() {
    // arrange: a mix of resolved and unresolved origins, including a SELECT *
    // that resolves as a load rather than an inferable column list
    let source = r#"
import pandas as pd

good = pd.read_csv("a.csv", usecols=["a"])
bare = pd.read_csv("b.csv")
star = pd.read_sql("SELECT * FROM t", conn)
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert: the invariant term-missing reporting depends on -- every origin
    // counted in the denominator but not the numerator has a recorded site
    assert_eq!(
        linter.untyped_sites.len(),
        linter.dataframes_total - linter.dataframes_typed
    );
    assert!(linter.dataframes_typed > 0);
    assert!(!linter.untyped_sites.is_empty());
}

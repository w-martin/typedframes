use super::*;

#[test]
fn test_should_report_an_untracked_frame_by_its_name() {
    let source = r#"
def run(client):
    rows = client.fetch("q")
    return rows["price"].sum()
"#;
    let linter = linter_for(source);
    let name = untracked_all(source, &linter)[0].name.clone();

    let single = report_for(source, &linter, &name).expect("a report");

    assert_eq!(single.verdict.outcome, "untracked");
    assert_eq!(single.name, name);
}

#[test]
fn test_should_not_report_an_object_construction_as_an_untracked_frame() {
    let source = r#"
def run(raw):
    sut = Service()
    return sut.transform(raw)
"#;
    let linter = linter_for(source);

    let reports = untracked_all(source, &linter);

    assert!(reports.is_empty(), "{reports:?}");
}

#[test]
fn test_should_not_keep_an_unrelated_class_that_shares_a_method_name() {
    let source = r#"
import pandas as pd


class Unrelated:
    def merge(self, other):
        return other


def run(a):
    frame = pd.read_csv("x.csv")
    a.merge(frame)
    return frame["x"].sum()
"#;

    let report = report(source, "frame");

    let snippet = report.snippet.expect("a snippet");
    assert!(!snippet.contains("class"), "{snippet}");
}

#[test]
fn test_should_stand_in_for_an_imported_function_by_signature_only() {
    // arrange
    let source = "from lib_private import load_orders\n\n\ndef run():\n    df = load_orders()\n    return df[\"a\"]\n";
    let offset = source.find("df =").unwrap();
    let mut stubs = HashMap::new();
    stubs.insert("load_orders".to_string(), Library::Pandas);

    // act
    let (snippet, _position) = build_snippet(source, offset, &stubs, &HashMap::new()).unwrap();

    // assert
    assert!(snippet.contains("-> pd.DataFrame: ..."), "{snippet}");
    assert!(snippet.contains("import pandas as pd"), "{snippet}");
    assert!(!snippet.contains("load_orders"), "{snippet}");
    assert!(!snippet.contains("lib_private"), "{snippet}");
}

#[test]
fn test_should_stand_in_for_an_imported_class_by_its_frame_methods_only() {
    // arrange
    let source = "from lib_private import OrderRepo\n\n\ndef run(repo: OrderRepo):\n    df = repo.load_orders()\n    return df[\"a\"]\n";
    let offset = source.find("df =").unwrap();
    let mut class_stubs = HashMap::new();
    class_stubs.insert(
        ("OrderRepo".to_string(), "load_orders".to_string()),
        Library::Polars,
    );

    // act
    let (snippet, _position) =
        build_snippet(source, offset, &HashMap::new(), &class_stubs).unwrap();

    // assert
    assert!(snippet.contains("class C1:"), "{snippet}");
    assert!(
        snippet.contains("(self, *args, **kwargs) -> pl.DataFrame: ..."),
        "{snippet}"
    );
    assert!(snippet.contains("import polars as pl"), "{snippet}");
    assert!(!snippet.contains("OrderRepo"), "{snippet}");
    assert!(!snippet.contains("load_orders"), "{snippet}");
    assert!(!snippet.contains("lib_private"), "{snippet}");
}

#[test]
fn test_should_mask_names_quotes_and_numbers_in_a_verdict() {
    assert_eq!(
        mask("only 'amount', 'region' are read later (lines 6, 8)"),
        "only '_', '_' are read later (lines N, N)"
    );
    assert_eq!(
        mask("passed to `clean()` (line 14)"),
        "passed to `_` (line N)"
    );
    assert_eq!(
        mask("can't see everything done with it: x"),
        "can't see everything done with it: x"
    );
}

#[test]
fn test_should_keep_library_and_checker_wording_in_a_verdict() {
    // arrange
    let unknown_library = "can't tell whether it is pandas or polars: the value is built \
             from `repo`, not a direct `pd.`/`pl.` call, a call to a function annotated \
             `-> pd.DataFrame` or `-> pl.DataFrame`, or a method like `.to_df()`";

    // act
    let masked = mask(unknown_library);

    // assert
    assert_eq!(
        masked,
        "can't tell whether it is pandas or polars: the value is built from `_`, not a \
             direct `pd.`/`pl.` call, a call to a function annotated `-> pd.DataFrame` or \
             `-> pl.DataFrame`, or a method like `.to_df()`"
    );
    assert_eq!(mask("built from `self`"), "built from `self`");
    assert_eq!(
        mask("`.columns` (line 11); `.reset_index()` (line 22)"),
        "`.columns` (line N); `.reset_index()` (line N)"
    );
    assert_eq!(
        mask("`.drop(inplace=True)` is not modelled"),
        "`.drop(inplace=True)` is not modelled"
    );
}

#[test]
fn test_should_mask_backticked_code_holding_a_user_name_string_or_number() {
    // act / assert
    assert_eq!(mask("`.price` (line 3)"), "`_` (line N)");
    assert_eq!(mask("`clean()` (line 3)"), "`_` (line N)");
    assert_eq!(mask("callers of `load`"), "callers of `_`");
    assert_eq!(mask("`keep()`"), "`_`");
    assert_eq!(mask("`self.revenue_store`"), "`_`");
    assert_eq!(mask("`df['price']`"), "`_`");
    assert_eq!(mask("`.iloc[5]`"), "`_`");
    assert_eq!(mask("`.agg(revenue=None)`"), "`_`");
}

#[test]
fn test_should_find_a_site_on_the_first_line_of_a_file_with_a_byte_order_mark() {
    // arrange
    let source = "\u{feff}import pandas as pd; df = pd.read_csv(\"x.csv\")\nprint(df[\"a\"])\n";
    let linter = linter_for(source);
    let site = &linter.untyped_sites[0];

    // act
    let offset = phrase::offset_of(
        source,
        &LineIndex::from_source_text(source),
        site.line,
        site.col,
    )
    .unwrap();

    // assert
    assert!(
        source[offset..].starts_with("df ="),
        "{}",
        &source[offset..]
    );
}

#[test]
fn test_should_report_every_untyped_site_in_a_file() {
    // arrange
    let source = "import pandas as pd\n\n\ndef one():\n    a = pd.read_csv(\"x.csv\")\n    return a[\"x\"]\n\n\ndef two():\n    b = pd.read_csv(\"y.csv\")\n    print(b[\"y\"])\n    return keep(b)\n";
    let linter = linter_for(source);

    // act
    let reports = report_all(source, &linter);

    // assert
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0].var, "a");
    assert_eq!(reports[1].var, "b");
    assert!(reports.iter().all(|r| r.snippet.is_some()));
}

#[test]
fn test_should_keep_an_import_the_remaining_code_still_uses_and_drop_one_it_does_not() {
    // arrange
    let source = "import numpy as np\nimport pandas as pd\n\n\nclass S:\n    def make(self, rows) -> pd.DataFrame:\n        return pd.DataFrame(rows)\n\n    def use(self):\n        n = np.zeros(3)\n        df = self.make([])\n        return df[\"a\"], n\n";

    // act
    let snippet = report(source, "df").snippet.expect("snippet");

    // assert
    assert!(snippet.contains("import pandas as pd"), "{snippet}");
    assert!(!snippet.contains("numpy"), "{snippet}");
}

#[test]
fn test_should_return_nothing_for_a_name_the_file_does_not_have() {
    // arrange
    let source = "import pandas as pd\ndf = pd.read_csv(\"x.csv\")\nprint(df[\"a\"])\n";
    let linter = linter_for(source);

    // act / assert
    assert!(report_for(source, &linter, "no-such").is_none());
}

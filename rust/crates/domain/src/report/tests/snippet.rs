use super::*;

#[test]
fn test_should_keep_the_helper_the_scope_calls_and_the_class_around_it() {
    // act
    let snippet = unreduced(PRIVATE, "revenue");

    // assert
    assert!(snippet.contains("class C1"), "{snippet}");
    assert_eq!(snippet.matches("def ").count(), 2, "{snippet}");
    assert!(snippet.contains("import pandas as pd"));
    assert!(!snippet.contains("import acme"));
}

#[test]
fn test_should_give_a_snippet_that_is_classified_like_the_original() {
    // act
    let report = report(PRIVATE, "revenue");

    // assert
    let snippet = report.snippet.expect("snippet");
    let redacted = linter_for(&snippet);
    assert_eq!(redacted.untyped_sites.len(), 1);
    let again = verdict_of(&redacted, &redacted.untyped_sites[0].phrase).unwrap();
    assert_eq!(again, report.verdict);
    assert!(report.refusal.is_none());
}

#[test]
fn test_should_report_a_withheld_site_with_a_masked_reason() {
    // arrange
    let source = "import pandas as pd\n\n\ndef run():\n    raw = pd.read_csv(\"x.csv\")\n    print(raw[\"a\"])\n    return clean_the_data(raw)\n";

    // act
    let report = report(source, "raw");

    // assert
    assert_eq!(report.verdict.outcome, "withheld");
    assert_eq!(
            report.verdict.reason,
            "can't see everything done with it: passed to `_`, a name this file never defines or imports (a builtin, or from a star import) (line N)"
        );
    assert!(!report.verdict.reason.contains("clean_the_data"));
    assert!(report.snippet.is_some());
}

#[test]
fn test_should_refuse_a_snippet_that_is_classified_differently() {
    // arrange: the withheld site's blocker removed makes it suggestible
    let source = "import pandas as pd\n\n\ndef run():\n    raw = pd.read_csv(\"x.csv\")\n    print(raw[\"a\"])\n    return clean(raw)\n";
    let real = report(source, "raw").verdict;
    let tampered = "import pandas as pd\n\n\ndef run():\n    raw = pd.read_csv(\"x.csv\")\n    print(raw[\"a\"])\n    return 1\n";

    // act
    let result = verify(tampered, (5, 5), &real);

    // assert
    let error = result.expect_err("a different verdict must be refused");
    assert!(error.contains("classified differently"), "{error}");
}

#[test]
fn test_should_reduce_a_scope_too_large_to_paste_to_the_code_that_decides_the_verdict() {
    // arrange
    let mut source =
        String::from("import pandas as pd\n\n\ndef run():\n    raw = pd.read_csv(\"x.csv\")\n");
    for i in 0..200 {
        source.push_str(&format!("    print({i})\n"));
    }
    source.push_str("    return raw[\"a\"]\n");

    // act
    let report = report(&source, "raw");

    // assert
    let snippet = report.snippet.expect("a snippet once the noise is dropped");
    assert!(!snippet.contains("print"), "{snippet}");
    assert!(snippet.contains("read_csv"), "{snippet}");
    assert!(snippet.lines().count() < 10, "{snippet}");
}

#[test]
fn test_should_refuse_a_site_whose_own_statement_is_too_large_to_paste() {
    // arrange
    let mut source = String::from(
            "import pandas as pd\n\n\ndef run():\n    raw = pd.read_csv(\n        \"x.csv\",\n        na_values=[\n",
        );
    for i in 0..200 {
        source.push_str(&format!("            \"n{i}\",\n"));
    }
    source.push_str("        ],\n    )\n    return raw[\"a\"]\n");

    // act
    let report = report(&source, "raw");

    // assert
    assert!(report.snippet.is_none());
    assert!(report.refusal.unwrap().contains("trim it by hand"));
}

#[test]
fn test_should_drop_code_that_does_not_decide_the_verdict() {
    // act
    let snippet = report(PRIVATE, "revenue").snippet.unwrap();

    // assert: the load and the call it escapes into stay; the arithmetic does not
    assert!(snippet.contains("read_csv"), "{snippet}");
    assert!(!snippet.contains('+'), "{snippet}");
    assert!(!snippet.contains("groupby"), "{snippet}");
    assert!(!snippet.contains("sum"), "{snippet}");
}

#[test]
fn test_should_keep_the_use_that_withholds_a_site_and_drop_the_rest() {
    // arrange
    let source = "import pandas as pd\n\n\ndef run(a, b):\n    raw = pd.read_csv(\"x.csv\")\n    print(raw[\"a\"])\n    return clean_the_data(raw)\n";

    // act
    let report = report(source, "raw");

    // assert
    assert_eq!(report.verdict.outcome, "withheld");
    let snippet = report.snippet.expect("snippet");
    assert!(!snippet.contains("print"), "{snippet}");
    assert_eq!(snippet.matches("(v").count(), 1, "{snippet}");
    assert!(
        snippet.contains("def f1():"),
        "unused parameters go: {snippet}"
    );
}

#[test]
fn test_should_give_a_reduced_snippet_that_is_still_classified_like_the_original() {
    // act
    let report = report(PRIVATE, "revenue");

    // assert
    let snippet = report.snippet.expect("snippet");
    let redacted = linter_for(&snippet);
    assert_eq!(redacted.untyped_sites.len(), 1);
    let again = verdict_of(&redacted, &redacted.untyped_sites[0].phrase).unwrap();
    assert!(same_verdict(&again, &report.verdict));
}

#[test]
fn test_should_keep_the_class_whose_method_a_local_instance_calls() {
    let source = r#"
import pandas as pd


class Service:
    def predict(self, rows) -> pd.DataFrame:
        return pd.DataFrame(rows)

    def unrelated(self):
        return 1


def run():
    sut = Service()
    frame = sut.predict([1])
    return frame["score"].sum()
"#;

    let report = report(source, "frame");

    assert_eq!(report.refusal, None);
    let snippet = report.snippet.expect("a snippet");
    assert!(snippet.contains("-> pd.DataFrame"), "{snippet}");
    assert!(!snippet.contains("unrelated"), "{snippet}");
}

#[test]
fn test_should_report_a_frame_the_checker_does_not_track() {
    let source = r#"
def run(client):
    rows = client.fetch("q")
    return rows["price"].sum()
"#;
    let linter = linter_for(source);

    let reports = untracked_all(source, &linter);

    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].verdict.outcome, "untracked");
    assert!(reports[0]
        .verdict
        .reason
        .contains("a method called on a local, parameter or imported module"));
    assert!(linter.untyped_sites.is_empty());
    let snippet = reports[0].snippet.as_deref().expect("a snippet");
    assert!(
        snippet.contains(".fetch(") && !snippet.contains("price"),
        "{snippet}"
    );
}

#[test]
fn test_should_keep_an_imported_callee_imported_in_the_snippet() {
    let source = r#"
import pandas as pd
from app.helpers import clean


def run():
    orders = pd.read_csv("orders.csv")
    clean(orders)
    return orders["total"].sum()
"#;
    let linter = linter_for(source);

    let reports = report_all(source, &linter);

    assert_eq!(reports.len(), 1, "{reports:?}");
    assert!(
        reports[0].verdict.reason.contains("an imported function"),
        "{reports:?}"
    );
    let snippet = reports[0].snippet.as_deref().unwrap_or_else(|| {
        panic!("no snippet: {:?}", reports[0].refusal);
    });
    assert!(
        snippet.contains("from m1 import f1\n") && !snippet.contains("helpers"),
        "{snippet}"
    );
}

#[test]
fn test_should_import_rather_than_stub_a_function_the_frame_is_passed_to() {
    // arrange: the index knows `clean` returns a pandas frame, so it would be stubbed
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    std::fs::write(root.join("pyproject.toml"), "").unwrap();
    std::fs::create_dir_all(root.join("app")).unwrap();
    std::fs::write(root.join("app/__init__.py"), "").unwrap();
    std::fs::write(
            root.join("app/helpers.py"),
            "import pandas as pd\n\n\ndef clean(df: pd.DataFrame) -> pd.DataFrame:\n    df.to_csv(\"out.csv\")\n    return df\n",
        )
        .unwrap();
    let source = "import pandas as pd\n\nfrom app.helpers import clean\n\n\ndef run() -> None:\n    orders = pd.read_csv(\"orders.csv\")\n    clean(orders)\n    print(orders[\"total\"])\n";
    let main = root.join("app/main.py");
    std::fs::write(&main, source).unwrap();
    let index = crate::index::build_index_internal(&root);
    let mut linter = Linter::new();
    linter.collect_suggestions = true;
    linter.load_cross_file_symbols(&index, source, &main, &root);
    linter.check_file_internal(source, &main).unwrap();

    // act
    let reports = report_all(source, &linter);

    // assert
    assert_eq!(reports.len(), 1, "{reports:?}");
    let snippet = reports[0].snippet.as_deref().unwrap_or_else(|| {
        panic!("no snippet: {:?}", reports[0].refusal);
    });
    assert!(snippet.contains("from m1 import f1\n"), "{snippet}");
}

#[test]
fn test_should_keep_a_test_function_a_test_in_the_snippet() {
    let source = r#"
import pandas as pd


def test_orders():
    orders = pd.read_csv("orders.csv")
    assert orders["total"].sum() > 0
"#;
    let linter = linter_for(source);

    let reports = report_all(source, &linter);

    assert_eq!(reports.len(), 1, "{reports:?}");
    assert!(reports[0].verdict.reason.contains("inside a test"));
    let snippet = reports[0].snippet.as_deref().unwrap_or_else(|| {
        panic!("no snippet: {:?}", reports[0].refusal);
    });
    assert!(
        snippet.contains("def test_f1(") && !snippet.contains("orders"),
        "{snippet}"
    );
}

#[test]
fn test_should_count_masked_columns_rather_than_list_them() {
    assert_eq!(
        count_hidden_columns("only '_', '_', '_' are read later; '_' is renamed"),
        "only 3 columns are read later; '_' is renamed"
    );
    assert_eq!(count_hidden_columns("'_', '_'"), "2 columns");
    assert_eq!(count_hidden_columns("no columns"), "no columns");
}

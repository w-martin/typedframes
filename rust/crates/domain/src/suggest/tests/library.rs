use super::*;

#[test]
fn test_should_follow_an_import_alias_to_the_library() {
    let source = "import pandas as p\ndf = p.read_json(\"x\")\nprint(df[\"a\"])\n";
    assert_eq!(selection_of(source), "df = df[[\"a\"]]\n");
}

#[test]
fn test_should_follow_a_name_imported_from_a_library() {
    let source = "from polars import read_json\ndf = read_json(\"x\")\nprint(df[\"a\"])\n";
    assert_eq!(selection_of(source), "df = df.select(\"a\")\n");
}

#[test]
fn test_should_read_the_library_from_the_method_that_produced_the_frame() {
    for (call, expected) in [
        (
            "store.get_historical_features(df).to_df()",
            "df = df[[\"a\"]]\n",
        ),
        ("cursor.fetch_pandas_all()", "df = df[[\"a\"]]\n"),
        ("spark.sql(q).toPandas()", "df = df[[\"a\"]]\n"),
        ("duckdb.sql(q).df()", "df = df[[\"a\"]]\n"),
        ("duckdb.sql(q).pl()", "df = df.select(\"a\")\n"),
        ("table.to_polars()", "df = df.select(\"a\")\n"),
    ] {
        let source = format!("df = {call}\nprint(df[\"a\"])\n");
        assert_eq!(selection_of(&source), expected, "{call}");
    }
}

#[test]
fn test_should_default_connectorx_to_pandas_and_honour_return_type() {
    let pandas = "import connectorx as cx\ndf = cx.read_sql(uri, q)\nprint(df[\"a\"])\n";
    let polars = "import connectorx as cx\ndf = cx.read_sql(uri, q, return_type=\"polars\")\nprint(df[\"a\"])\n";
    assert_eq!(selection_of(pandas), "df = df[[\"a\"]]\n");
    assert_eq!(selection_of(polars), "df = df.select(\"a\")\n");
}

#[test]
fn test_should_read_the_library_from_a_functions_return_annotation() {
    for (annotation, expected) in [
        ("pd.DataFrame", "df = df[[\"a\"]]\n"),
        ("pl.LazyFrame", "df = df.select(\"a\")\n"),
        ("pl.DataFrame | None", "df = df.select(\"a\")\n"),
        ("Optional[pd.DataFrame]", "df = df[[\"a\"]]\n"),
    ] {
        let source = format!(
                "from typing import Optional\ndef load() -> {annotation}:\n    ...\ndf = load()\nprint(df[\"a\"])\n"
            );
        assert_eq!(selection_of(&source), expected, "{annotation}");
    }
}

#[test]
fn test_should_withhold_a_function_with_no_or_conflicting_return_annotations() {
    let unannotated = "def load():\n    ...\ndf = load()\nprint(df[\"a\"])\n";
    assert!(run(unannotated, true)
        .unwrap_err()
        .contains("pandas or polars"));
    let conflicting = "def load() -> pd.DataFrame:\n    ...\n\n\nclass A:\n    def load(self) -> pl.DataFrame:\n        ...\n\n\ndf = load()\nprint(df[\"a\"])\n";
    assert!(run(conflicting, true).is_err());
}

#[test]
fn test_should_withhold_a_frame_from_an_unknown_system() {
    let source = "df = spark.sql(q)\nprint(df[\"a\"])\n";
    let reason = run(source, true).unwrap_err();
    assert!(reason.contains("`spark`"), "{reason}");
}

#[test]
fn test_should_prefer_the_variables_own_annotation() {
    let source = "df: pl.DataFrame = fetch()\nprint(df[\"a\"])\n";
    let module = parse_module(source).unwrap().into_syntax();
    let Stmt::AnnAssign(ann) = &module.body[0] else {
        panic!()
    };
    let usage = scan_usage("df", &module.body[1..], &|_| 2);
    let libraries = test_libraries();
    let s = suggest_selection(
        &Origin {
            libraries: &libraries,
            annotation: Some(&ann.annotation),
            source,
            display_path: "f.py",
            value: ann.value.as_deref(),
            var: "df",
            line: 1,
            col: 1,
            stmt_start: 0,
            stmt_end: source.lines().next().unwrap().len(),
            function_local: true,
            receivers: &HashMap::new(),
            known_columns: &[],
            scope_body: &[],
        },
        &usage,
    )
    .unwrap();
    assert!(s.diff.contains(".select(\"a\")"));
}

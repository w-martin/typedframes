use super::*;

#[test]
fn test_should_select_columns_with_a_pandas_subscript() {
    let source = "df = pd.read_json(\"x.parquet\")\nprint(df[\"a\"], df[\"b\"])\n";
    let s = run(source, true).unwrap();

    assert_eq!(s.kind, "select");
    assert_eq!(s.tier, "safe");
    assert_eq!(s.columns, vec!["a", "b"]);
    assert_eq!(
        applied(source, &s),
        "df = pd.read_json(\"x.parquet\")\ndf = df[[\"a\", \"b\"]]\nprint(df[\"a\"], df[\"b\"])\n"
    );
    assert!(s.diff.contains("+df = df[[\"a\", \"b\"]]"));
}

#[test]
fn test_should_select_columns_with_a_polars_select() {
    let source = "df = pl.read_json(\"x.parquet\")\nprint(df[\"a\"])\n";
    let s = run(source, true).unwrap();

    assert!(applied(source, &s).contains("df = df.select(\"a\")\n"));
}

#[test]
fn test_should_follow_a_method_chain_back_to_the_module() {
    let source = "df = pl.scan_csv(\"x.csv\").collect()\nprint(df[\"a\"])\n";
    assert!(run(source, true).unwrap().diff.contains(".select(\"a\")"));
    let source = "df = pd.read_csv(\"x.csv\").dropna()\nprint(df[\"a\"])\n";
    assert!(run(source, true).unwrap().diff.contains("df[[\"a\"]]"));
}

#[test]
fn test_should_dedupe_columns_in_first_read_order() {
    let source = "df = pd.read_json(\"x\")\nprint(df[\"b\"], df[\"a\"], df[\"b\"])\n";
    assert_eq!(run(source, true).unwrap().columns, vec!["b", "a"]);
}

#[test]
fn test_should_keep_the_indentation_of_the_origin() {
    let source = "def f():\n    df = pd.read_json(\"x\")\n    print(df[\"a\"])\n";
    let module = parse_module(source).unwrap().into_syntax();
    let Stmt::FunctionDef(f) = &module.body[0] else {
        panic!()
    };
    let Stmt::Assign(assign) = &f.body[0] else {
        panic!()
    };
    let usage = scan_usage("df", &f.body[1..], &|_| 3);
    let libraries = test_libraries();
    let s = suggest_selection(
        &Origin {
            libraries: &libraries,
            annotation: None,
            source,
            display_path: "f.py",
            value: Some(&assign.value),
            var: "df",
            line: 2,
            col: 5,
            stmt_start: assign.range().start().to_usize(),
            stmt_end: assign.range().end().to_usize(),
            function_local: true,
            receivers: &HashMap::new(),
            known_columns: &[],
            scope_body: &[],
        },
        &usage,
    )
    .unwrap();
    assert_eq!(
        applied(source, &s),
        "def f():\n    df = pd.read_json(\"x\")\n    df = df[[\"a\"]]\n    print(df[\"a\"])\n"
    );
}

#[test]
fn test_should_insert_after_a_multiline_origin_and_its_comment() {
    let source = "df = pd.read_json(\n    \"x\",\n)  # load\nprint(df[\"a\"])\n";
    let s = run(source, true).unwrap();
    assert_eq!(
        applied(source, &s),
        "df = pd.read_json(\n    \"x\",\n)  # load\ndf = df[[\"a\"]]\nprint(df[\"a\"])\n"
    );
}

#[test]
fn test_should_add_a_newline_when_the_origin_is_the_last_line() {
    let source = "df = pd.read_json(\"x\")";
    let module = parse_module(source).unwrap().into_syntax();
    let Stmt::Assign(assign) = &module.body[0] else {
        panic!()
    };
    let usage = ColumnUsage {
        accesses: vec![crate::column_usage::ColumnAccess {
            column: "a".to_string(),
            conditional: false,
            line: 1,
            via: None,
        }],
        ..ColumnUsage::default()
    };
    let libraries = test_libraries();
    let s = suggest_selection(
        &Origin {
            libraries: &libraries,
            annotation: None,
            source,
            display_path: "f.py",
            value: Some(&assign.value),
            var: "df",
            line: 1,
            col: 1,
            stmt_start: 0,
            stmt_end: source.len(),
            function_local: true,
            receivers: &HashMap::new(),
            known_columns: &[],
            scope_body: &[],
        },
        &usage,
    )
    .unwrap();
    assert_eq!(
        applied(source, &s),
        "df = pd.read_json(\"x\")\ndf = df[[\"a\"]]"
    );
}

#[test]
fn test_should_withhold_when_other_code_shares_the_line() {
    let source = "df = pd.read_json(\"x\"); b = 2\nprint(df[\"a\"])\n";
    let reason = run(source, true).unwrap_err();
    assert!(reason.contains("shares a line"), "{reason}");
}

#[test]
fn test_should_add_usecols_to_a_pandas_read_csv() {
    let source = "df = pd.read_csv(\"x.csv\", sep=\";\")\nprint(df[\"a\"], df[\"b\"])\n";
    let suggestion = run(source, true).unwrap();
    assert_eq!(suggestion.kind, "usecols");
    assert_eq!(suggestion.tier, "safe");
    assert_eq!(
            applied(source, &suggestion),
            "df = pd.read_csv(\"x.csv\", sep=\";\", usecols=[\"a\", \"b\"])\nprint(df[\"a\"], df[\"b\"])\n"
        );
    assert!(suggestion.diff.contains("+df = pd.read_csv"));
}

#[test]
fn test_should_add_columns_to_a_read_parquet_and_a_polars_read_csv() {
    for (source, expected) in [
        (
            "df = pd.read_parquet(\"x\")\nprint(df[\"a\"])\n",
            "df = pd.read_parquet(\"x\", columns=[\"a\"])\nprint(df[\"a\"])\n",
        ),
        (
            "df = pl.read_csv(\"x\")\nprint(df[\"a\"])\n",
            "df = pl.read_csv(\"x\", columns=[\"a\"])\nprint(df[\"a\"])\n",
        ),
        (
            "df = pl.read_parquet(\"x\")\nprint(df[\"a\"])\n",
            "df = pl.read_parquet(\"x\", columns=[\"a\"])\nprint(df[\"a\"])\n",
        ),
    ] {
        let suggestion = run(source, true).unwrap();
        assert_eq!(suggestion.kind, "columns", "{source}");
        assert_eq!(applied(source, &suggestion), expected);
    }
}

#[test]
fn test_should_add_the_column_list_after_the_last_argument_of_a_multiline_call() {
    let source = "df = pd.read_csv(\n    \"x\",\n    sep=\";\",\n)\nprint(df[\"a\"])\n";
    let suggestion = run(source, true).unwrap();
    assert_eq!(
        applied(source, &suggestion),
        "df = pd.read_csv(\n    \"x\",\n    sep=\";\", usecols=[\"a\"],\n)\nprint(df[\"a\"])\n"
    );
    parse_module(&applied(source, &suggestion)).expect("the edited source still parses");
}

#[test]
fn test_should_add_usecols_to_a_read_csv_imported_by_name() {
    let source = "from pandas import read_csv\ndf = read_csv(\"x\")\nprint(df[\"a\"])\n";
    let suggestion = run(source, true).unwrap();
    assert_eq!(suggestion.kind, "usecols");
}

#[test]
fn test_should_select_after_the_load_when_the_call_has_other_options() {
    for source in [
        "df = pd.read_csv(\"x\", parse_dates=[\"a\"])\nprint(df[\"a\"])\n",
        "df = pd.read_csv(\"x\", names=[\"a\", \"b\"])\nprint(df[\"a\"])\n",
        "df = pd.read_csv(\"x\", usecols=lambda c: True)\nprint(df[\"a\"])\n",
        "df = pd.read_csv(\"x\", **options)\nprint(df[\"a\"])\n",
        "df = pd.read_csv(*paths)\nprint(df[\"a\"])\n",
        "df = pd.read_csv(\"x\", \"y\")\nprint(df[\"a\"])\n",
        "df = pd.read_csv(\"x\").copy()\nprint(df[\"a\"])\n",
    ] {
        let suggestion = run(source, true).unwrap_or_else(|e| panic!("{source}: {e}"));
        assert_eq!(suggestion.kind, "select", "{source}");
    }
}

#[test]
fn test_should_add_the_column_list_outside_a_parenthesised_argument() {
    for (source, expected) in [
        (
            "df = pd.read_csv((path))\nprint(df[\"a\"])\n",
            "df = pd.read_csv((path), usecols=[\"a\"])\nprint(df[\"a\"])\n",
        ),
        (
            "df = pd.read_csv(\"x\", sep=(\";\"))\nprint(df[\"a\"])\n",
            "df = pd.read_csv(\"x\", sep=(\";\"), usecols=[\"a\"])\nprint(df[\"a\"])\n",
        ),
        (
            "df = pd.read_csv(((path)) )\nprint(df[\"a\"])\n",
            "df = pd.read_csv(((path)), usecols=[\"a\"] )\nprint(df[\"a\"])\n",
        ),
    ] {
        let suggestion = run(source, true).unwrap();
        let edited = applied(source, &suggestion);
        assert_eq!(edited, expected);
        parse_module(&edited).expect("the edited source still parses");
    }
}

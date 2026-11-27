use super::*;

#[test]
fn test_should_fail_the_global_bucket_below_its_threshold() {
    // arrange
    let per_file = tallies(&[("/p/a.py", 1, 4), ("/p/b.py", 2, 4)]);

    // act
    let failing = evaluate_coverage(&per_file, &config(50.0, &[]), Path::new("/p"), None);

    // assert
    assert_eq!(1, failing.len());
    assert_eq!(
        (3, 8, None),
        (failing[0].typed, failing[0].total, failing[0].label.clone())
    );
}

#[test]
fn test_should_pass_a_bucket_with_no_dataframes() {
    // arrange
    let per_file = tallies(&[("/p/a.py", 0, 0)]);

    // act
    let failing = evaluate_coverage(&per_file, &config(100.0, &[]), Path::new("/p"), None);

    // assert
    assert!(failing.is_empty());
}

#[test]
fn test_should_grade_a_legacy_subtree_on_its_own_override() {
    // arrange
    let per_file = tallies(&[("/p/legacy/a.py", 1, 4), ("/p/new/b.py", 4, 4)]);
    let settings = config(90.0, &[("legacy/**", 20.0)]);

    // act
    let failing = evaluate_coverage(&per_file, &settings, Path::new("/p"), None);

    // assert
    assert!(failing.is_empty());
}

#[test]
fn test_should_let_the_command_line_threshold_ignore_overrides() {
    // arrange
    let per_file = tallies(&[("/p/legacy/a.py", 1, 4)]);
    let settings = config(90.0, &[("legacy/**", 20.0)]);

    // act
    let failing = evaluate_coverage(&per_file, &settings, Path::new("/p"), Some(100.0));

    // assert
    assert_eq!(1, failing.len());
    assert_eq!(None, failing[0].label);
}

#[test]
fn test_should_pick_the_most_specific_matching_override() {
    // arrange
    let per_file = tallies(&[("/p/src/new_module/a.py", 1, 2)]);
    let settings = config(100.0, &[("src/**", 10.0), ("src/new_module/**", 90.0)]);

    // act
    let failing = evaluate_coverage(&per_file, &settings, Path::new("/p"), None);

    // assert
    assert_eq!(Some("src/new_module/**".to_string()), failing[0].label);
}

#[test]
fn test_should_put_the_global_failure_before_override_failures() {
    // arrange
    let per_file = tallies(&[("/p/b/x.py", 0, 2), ("/p/a/y.py", 0, 2), ("/p/z.py", 0, 2)]);
    let settings = config(50.0, &[("b/**", 50.0), ("a/**", 50.0)]);

    // act
    let failing = evaluate_coverage(&per_file, &settings, Path::new("/p"), None);

    // assert
    let labels: Vec<Option<String>> = failing.iter().map(|b| b.label.clone()).collect();
    assert_eq!(vec![None, Some("a/**".into()), Some("b/**".into())], labels);
}

#[test]
fn test_should_show_one_decimal_and_the_scope_in_a_failure() {
    // arrange
    let bucket = Bucket {
        label: Some("legacy/**".into()),
        threshold: 100.0,
        total: 1000,
        typed: 996,
    };

    // act
    let message = failure_message(&bucket);

    // assert
    assert_eq!(
        "\u{2717} DataFrame schema coverage 99.6% is below the required 100.0% for 'legacy/**' (996/1000 DataFrames had column info)",
        message
    );
}

#[test]
fn test_should_list_missing_sites_in_a_term_missing_table() {
    // arrange
    let per_file = tallies(&[("/p/a.py", 1, 3), ("/p/empty.py", 0, 0)]);
    let sites = vec![
        json!({"file": "/p/a.py", "var": "df2", "line": 9, "col": 1}),
        json!({"file": "/p/a.py", "var": "df1", "line": 3, "col": 1}),
    ];

    // act
    let table = format_term_missing(&per_file, &sites, Path::new("/p"));

    // assert
    let expected = "\
Name  Typed  Total   Cover   Missing
------------------------------------
a.py      1      3     33%   df1:3, df2:9
------------------------------------
TOTAL      1      3     33%";
    assert_eq!(expected, table);
}

#[test]
fn test_should_say_so_when_no_file_has_dataframes() {
    // arrange
    let per_file = tallies(&[("/p/a.py", 0, 0)]);

    // act
    let table = format_term_missing(&per_file, &[], Path::new("/p"));

    // assert
    assert_eq!(
        "No DataFrames with recognized loads/schemas found to check",
        table
    );
}

#[test]
fn test_should_leave_percent_null_for_a_file_without_dataframes() {
    // arrange
    let per_file = tallies(&[("/p/a.py", 0, 0)]);

    // act
    let payload = coverage_json(&per_file, &[], Path::new("/p"));

    // assert
    assert_eq!(Value::Null, payload["percent"]);
    assert_eq!(Value::Null, payload["files"][0]["percent"]);
    assert_eq!("a.py", payload["files"][0]["file"]);
}

#[test]
fn test_should_classify_each_call_in_the_explain_text() {
    // arrange
    let (calls, typed, untyped) = explain_sources_fixture();
    let sources = Sources {
        all_dataframe_calls: &calls,
        typed_sites: &typed,
        untyped_sites: &untyped,
        ..Sources::default()
    };

    // act
    let report = format_explain(&sources, Path::new("/p"));

    // assert
    let expected = "\
a.py
  2:5          pd.read_csv      COUNTED, typed (schema: Orders)
  7:5          pd.read_csv      NOT COUNTED \u{2014} a bare return
  9:5          pd.read_json     COUNTED, untracked -- no static column info  [quiet-amber]";
    assert_eq!(expected, report);
}

#[test]
fn test_should_not_match_a_site_to_one_on_the_same_line_of_another_cell() {
    // arrange: line 2 of cell 2 is untracked, line 2 of cell 3 is typed
    let calls = vec![
        json!({"file": "/p/n.ipynb", "cell": 2, "line": 2, "col": 10, "label": "pd.read_csv", "context": "assignment"}),
        json!({"file": "/p/n.ipynb", "cell": 3, "line": 2, "col": 7, "label": "pd.DataFrame", "context": "assignment"}),
    ];
    let typed = vec![json!({"file": "/p/n.ipynb", "cell": 3, "line": 2, "schema": "S"})];
    let untyped =
        vec![json!({"file": "/p/n.ipynb", "cell": 2, "line": 2, "col": 1, "var": "claims"})];
    let sources = Sources {
        all_dataframe_calls: &calls,
        typed_sites: &typed,
        untyped_sites: &untyped,
        ..Sources::default()
    };

    // act
    let report = format_explain(&sources, Path::new("/p"));

    // assert
    let row = |prefix: &str| report.lines().find(|l| l.trim_start().starts_with(prefix));
    assert!(
        row("cell 2:2:10").unwrap().contains("COUNTED, untracked"),
        "{report}"
    );
    assert!(
        row("cell 3:2:7")
            .unwrap()
            .contains("COUNTED, typed (schema: S)"),
        "{report}"
    );
}

#[test]
fn test_should_mirror_the_explain_text_in_the_explain_json() {
    // arrange
    let (calls, typed, untyped) = explain_sources_fixture();
    let sources = Sources {
        all_dataframe_calls: &calls,
        typed_sites: &typed,
        untyped_sites: &untyped,
        ..Sources::default()
    };

    // act
    let payload = explain_json(&sources, Path::new("/p"));

    // assert
    let entries = &payload["files"][0]["calls"];
    assert_eq!(json!(true), entries[0]["typed"]);
    assert_eq!(json!("Orders"), entries[0]["schema"]);
    assert_eq!(json!(false), entries[1]["counted"]);
    assert_eq!(json!(true), entries[2]["counted"]);
    let keys: Vec<&String> = entries[0].as_object().unwrap().keys().collect();
    assert_eq!(
        vec!["line", "col", "label", "counted", "typed", "schema", "context"],
        keys
    );
}

#[test]
fn test_should_collapse_repeated_usage_lines_with_a_count() {
    // arrange
    let usage = json!({
        "file": "/p/a.py", "line": 4, "col": 1, "var": "df",
        "accesses": [{"column": "a", "conditional": false}, {"column": "a", "conditional": false}],
        "other_uses": [{"description": "passed to f()", "conditional": true, "consumes": "all"}],
    });

    // act
    let lines = usage_lines(&usage);

    // assert
    assert_eq!(
        vec![
            "4:1          df               'a' (x2)".to_string(),
            "4:1          df               passed to f() (conditional) [touches every column]"
                .to_string(),
        ],
        lines
    );
}

#[test]
fn test_should_report_no_calls_found_when_there_is_nothing_to_explain() {
    assert_eq!(
        "No DataFrame-shaped calls found to check",
        format_explain(&Sources::default(), &PathBuf::from("/p"))
    );
}

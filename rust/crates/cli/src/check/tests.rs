use super::*;

fn error(file: &str, line: i64, severity: &str, code: &str) -> Value {
    json!({"line": line, "col": 5, "code": code, "message": "m", "severity": severity, "file": file})
}

#[test]
fn test_should_format_an_error_as_a_ty_style_line() {
    // arrange
    let errors = vec![error("a.py", 3, "error", "unknown-column")];

    // act
    let text = format_text(&errors, false);

    // assert
    assert_eq!("a.py:3:5: error[unknown-column] m", text);
}

#[test]
fn test_should_put_a_notebook_cell_before_the_position() {
    // arrange
    let mut error = error("n.ipynb", 3, "warning", "");
    error["cell"] = json!(2);

    // act
    let text = format_text(&[error], false);

    // assert
    assert_eq!("n.ipynb:cell 2:3:5: warning m", text);
}

#[test]
fn test_should_colour_each_severity() {
    // arrange
    let errors = vec![error("a.py", 1, "info", "c")];

    // act
    let text = format_text(&errors, true);

    // assert
    assert_eq!(
        "\u{1b}[1ma.py\u{1b}[0m:1:5: \u{1b}[2minfo\u{1b}[0m[c] m",
        text
    );
}

#[test]
fn test_should_map_info_to_a_github_notice_and_name_the_notebook_cell() {
    // arrange
    let mut info = error("a.py", 1, "info", "");
    let mut cell = error("n.ipynb", 2, "error", "unknown-column");
    cell["cell"] = json!(4);
    info["code"] = json!("");

    // act
    let text = format_github(&[info, cell]);

    // assert
    assert_eq!(
            "::notice file=a.py,line=1,col=5,title=info::m\n::error file=n.ipynb,line=2,col=5,title=unknown-column (notebook cell 4)::m",
            text
        );
}

#[test]
fn test_should_count_errors_and_warnings_in_the_summary() {
    // arrange
    let plain = error("a.py", 1, "error", "unknown-column");

    // act
    let only_plain = error_summary_parts(&[&plain, &plain], 1);
    let none = error_summary_parts(&[], 2);

    // assert
    assert_eq!(vec!["2 errors", "1 warning"], only_plain);
    assert_eq!(vec!["0 errors", "2 warnings"], none);
}

#[test]
fn test_should_lead_with_a_caveat_only_below_half_coverage_and_with_findings() {
    assert_eq!(None, low_coverage_caveat(10, 4, 0, 0));
    assert_eq!(None, low_coverage_caveat(10, 5, 0, 3));
    assert_eq!(None, low_coverage_caveat(0, 0, 0, 3));
    assert_eq!(
            Some("\u{2139} 1/4 DataFrames had column info (25%) \u{2014} the 1 finding below only covers what little could be tracked".to_string()),
            low_coverage_caveat(4, 1, 0, 1)
        );
}

#[test]
fn test_should_word_the_coverage_summary() {
    assert_eq!(
        "\u{2139} No DataFrames with recognized loads/schemas found to check",
        coverage_message(0, 0, 0, 0, 0)
    );
    assert_eq!(
            "\u{2139} 1/3 DataFrames had column info (33%) \u{2014} DataFrame schema coverage, not a pass/fail result",
            coverage_message(3, 1, 0, 0, 0)
        );
    assert_eq!(
            "\u{2139} 1/3 DataFrames had column info (33%), 2 annotated with an unresolved schema \u{2014} DataFrame schema coverage, not a pass/fail result",
            coverage_message(3, 1, 2, 0, 0)
        );
}

#[test]
fn test_should_say_how_many_loads_the_percentage_does_not_cover() {
    assert!(coverage_message(1, 1, 0, 0, 1).contains("not counting 1 load inside other"));
    assert!(coverage_message(1, 1, 0, 0, 2).contains("not counting 2 loads inside other"));
    assert!(!coverage_message(1, 1, 0, 0, 0).contains("not counting"));
}

#[test]
fn test_should_say_how_many_derived_frames_lost_their_columns() {
    assert_eq!(
            "\u{2139} 2/3 DataFrames had column info (67%), 1 derived frame lost its columns later (`--coverage-detail=explain` lists them) \u{2014} DataFrame schema coverage, not a pass/fail result",
            coverage_message(3, 2, 0, 1, 0)
        );
    assert!(coverage_message(3, 2, 0, 4, 0).contains("4 derived frames lost their columns"));
}

#[test]
fn test_should_count_each_lost_frame_once_per_file() {
    // arrange
    let findings = Findings {
        leg_events: vec![
            json!({"file": "a.py", "var": "x", "outcome": "untracked"}),
            json!({"file": "a.py", "var": "x", "outcome": "unresolved"}),
            json!({"file": "b.py", "var": "x", "outcome": "unresolved"}),
            json!({"file": "a.py", "var": "y", "outcome": "something else"}),
        ],
        ..Findings::default()
    };

    // act / assert
    assert_eq!(2, findings.lost_frames());
}

#[test]
fn test_should_switch_duration_units_after_rounding() {
    assert_eq!("999\u{b5}s", format_duration(0.000_999));
    assert_eq!("1ms", format_duration(0.000_999_9));
    assert_eq!("999ms", format_duration(0.999));
    assert_eq!("1.00s", format_duration(0.999_6));
}

#[test]
fn test_should_tag_each_site_with_its_file_last() {
    // arrange
    let site = json!({"var": "df", "line": 3});

    // act
    let tagged = tagged(&site, "a.py");

    // assert
    let keys: Vec<&String> = tagged.as_object().unwrap().keys().collect();
    assert_eq!(vec!["var", "line", "file"], keys);
}

#[test]
fn test_should_skip_a_file_the_checker_could_not_read_and_keep_the_rest() {
    // arrange
    let files = vec![PathBuf::from("a.py"), PathBuf::from("b.py")];
    let good =
        json!({"errors": [{"line": 1, "col": 1, "code": "c", "message": "m", "severity": "error"}],
                          "stats": {"dataframes_total": 2, "dataframes_typed": 1}})
        .to_string();
    let outcomes = vec![
        (None, Some("permission denied".to_string())),
        (Some(good), None),
    ];

    // act
    let findings = gather(&files, outcomes);

    // assert
    assert_eq!(1, findings.errors.len());
    assert_eq!("b.py", findings.errors[0]["file"]);
    assert_eq!((2, 1), (findings.total, findings.typed));
    assert_eq!(1, findings.per_file.len());
}

#[test]
fn test_should_downgrade_untracked_dataframe_to_info_and_filter_by_flag() {
    // arrange
    let errors = vec![
        error("a.py", 1, "warning", "untracked-dataframe"),
        error("a.py", 2, "warning", "dropped-unknown-column"),
        error("a.py", 3, "error", "unknown-column"),
    ];
    let lenient = CheckArgs::for_test(|a| a.lenient_ingest = true);
    let quiet = CheckArgs::for_test(|a| {
        a.lenient_ingest = true;
        a.no_warnings = true;
        a.no_info = true;
    });

    // act
    let downgraded = apply_diagnostic_policy(errors.clone(), &lenient);
    let filtered = apply_diagnostic_policy(errors, &quiet);

    // assert
    assert_eq!(Some("info"), severity(&downgraded[0]));
    assert_eq!(1, filtered.len());
    assert_eq!(Some("error"), severity(&filtered[0]));
}

#[test]
fn test_should_collapse_assumed_constant_notes_into_one_unless_verbose() {
    // arrange
    let errors = vec![
        error("a.py", 1, "info", "assumed-constant"),
        error("a.py", 2, "error", "unknown-column"),
        error("a.py", 3, "info", "assumed-constant"),
        error("b.py", 4, "info", "assumed-constant"),
    ];
    let quiet = CheckArgs::for_test(|_| {});
    let verbose = CheckArgs::for_test(|a| a.verbose = true);

    // act
    let collapsed = apply_diagnostic_policy(errors.clone(), &quiet);
    let listed = apply_diagnostic_policy(errors, &verbose);

    // assert
    assert_eq!(2, collapsed.len());
    assert_eq!("assumed-constant", str_of(&collapsed[0], "code"));
    assert!(str_of(&collapsed[0], "message").starts_with("3 `self.<NAME>` constants"));
    assert_eq!("unknown-column", str_of(&collapsed[1], "code"));
    assert_eq!(4, listed.len());
}

#[test]
fn test_should_leave_a_single_assumed_constant_note_untouched() {
    // arrange
    let errors = vec![error("a.py", 1, "info", "assumed-constant")];

    // act
    let result = apply_diagnostic_policy(errors, &CheckArgs::for_test(|_| {}));

    // assert
    assert_eq!("m", str_of(&result[0], "message"));
}

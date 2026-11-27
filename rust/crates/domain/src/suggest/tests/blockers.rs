use super::*;

#[test]
fn test_should_name_the_root_of_a_value_whose_library_is_unknown() {
    let reason = run("df = self.build()\nprint(df[\"a\"])\n", true).unwrap_err();
    assert!(reason.contains("built from `self`"), "{reason}");
}

#[test]
fn test_should_withhold_when_the_library_is_unknown() {
    for source in [
        "df = read_csv(\"x\")\nprint(df[\"a\"])\n",
        "df = load()\nprint(df[\"a\"])\n",
        "df = [1]\nprint(df[\"a\"])\n",
    ] {
        assert!(run(source, true).is_err(), "{source}");
    }
    let reason = run("df = load()\nprint(df[\"a\"])\n", true).unwrap_err();
    assert!(reason.contains("pandas or polars"), "{reason}");
}

#[test]
fn test_should_suggest_when_shape_and_head_are_only_displayed() {
    // arrange: nothing but display uses besides the column reads
    let source = "df = pd.read_csv(\"x.csv\")\nprint(df.shape)\nprint(df.head())\ndf.head()\nprint(df[\"a\"])\n";

    // act
    let suggestion = run(source, true).unwrap();

    // assert: narrowing goes ahead, with the display flagged for review
    assert!(
        suggestion.notes.iter().any(|n| n.contains("displayed")),
        "{:?}",
        suggestion.notes
    );
}

#[test]
fn test_should_name_the_use_that_blocks_not_a_shape_read_first() {
    // arrange
    let source = "df = pd.read_csv(\"x.csv\")\nn = df.shape[0]\nhelper(df)\nprint(df[\"a\"])\n";

    // act
    let reason = run(source, true).unwrap_err();

    // assert
    assert!(reason.contains("passed to"), "{reason}");
    assert!(reason.contains("line 3"), "{reason}");
}

#[test]
fn test_should_withhold_when_the_frame_is_passed_to_a_call() {
    let source = "df = pd.read_json(\"x\")\nhelper(df)\nprint(df[\"a\"])\n";
    let reason = run(source, true).unwrap_err();
    assert!(reason.contains("can't see everything"), "{reason}");
    assert!(reason.contains("line 2"), "{reason}");
}

#[test]
fn test_should_name_every_blocker_up_to_a_handful_then_count_the_rest() {
    let few = "df = pd.read_json(\"x\")\nhelper(df)\ndf.groupby(\"a\")\nprint(df[\"a\"])\n";
    let many = "df = pd.read_json(\"x\")\nh1(df)\nh2(df)\nh3(df)\nh4(df)\nh5(df)\nh6(df)\nprint(df[\"a\"])\n";

    let reason = run(few, true).unwrap_err();
    let overflow = run(many, true).unwrap_err();

    assert!(
        reason.contains("(line 2)") && reason.contains("(line 3)"),
        "{reason}"
    );
    assert!(!reason.contains("more"), "{reason}");
    assert!(overflow.ends_with(", and 2 more"), "{overflow}");
}

#[test]
fn test_should_not_be_blocked_by_index_empty_or_len() {
    let source = "df = pd.read_json(\"x\")\nprint(df.index, df.empty, len(df))\nprint(df[\"a\"])\n";
    assert!(run(source, true).is_ok());
}

#[test]
fn test_should_block_on_columns_because_narrowing_changes_it() {
    let source = "df = pd.read_json(\"x\")\nkeep = [c for c in df.columns]\nprint(df[\"a\"])\n";
    assert!(run(source, true).is_err());
}

#[test]
fn test_should_withhold_when_nothing_is_read() {
    let reason = run("df = pd.read_json(\"x\")\nprint(1)\n", true).unwrap_err();
    assert!(reason.contains("no column is read"), "{reason}");
}

#[test]
fn test_should_not_count_a_column_created_in_scope_as_a_source_column() {
    let source = "df = pd.read_json(\"x\")\ndf[\"total\"] = df[\"a\"] * 2\nprint(df[\"total\"], df[\"b\"])\n";
    let s = run(source, true).unwrap();
    assert_eq!(s.columns, vec!["a", "b"]);
}

#[test]
fn test_should_still_count_a_column_written_only_conditionally() {
    let source =
        "df = pd.read_json(\"x\")\nif flag:\n    df[\"c\"] = 1\nprint(df[\"c\"], df[\"a\"])\n";
    assert_eq!(run(source, true).unwrap().columns, vec!["c", "a"]);
}

#[test]
fn test_should_note_pandas_assignment_into_a_selection_for_review() {
    let source = "df = pd.read_json(\"x\")\ndf[\"t\"] = df[\"a\"]\nprint(df[\"a\"])\n";
    let s = run(source, true).unwrap();
    assert_eq!(s.tier, "review");
    assert!(s.notes[0].contains("SettingWithCopyWarning"));
    let polars = "df = pl.read_json(\"x\")\ndf[\"t\"] = df[\"a\"]\nprint(df[\"a\"])\n";
    assert_eq!(run(polars, true).unwrap().tier, "safe");
}

#[test]
fn test_should_mark_a_column_read_once_next_to_a_near_miss_as_review() {
    // arrange: `risk_calc` is read once and nowhere else, two edits from `risk_cal`
    let source =
        "df = pd.read_json(\"x\")\nprint(df[\"risk_cal\"], df[\"risk_cal\"], df[\"risk_calc\"])\n";

    // act
    let s = run(source, true).unwrap();

    // assert
    assert_eq!(s.tier, "review");
    assert!(s
        .notes
        .iter()
        .any(|n| n.contains("'risk_calc'") && n.contains("misspelling")));
}

#[test]
fn test_should_keep_distinct_columns_read_once_each_safe() {
    let source = "df = pd.read_json(\"x\")\nprint(df[\"age\"], df[\"income\"])\n";
    assert_eq!(run(source, true).unwrap().tier, "safe");
}

#[test]
fn test_should_mark_a_conditional_only_column_as_review() {
    let source = "df = pd.read_json(\"x\")\nif flag:\n    print(df[\"b\"])\nprint(df[\"a\"])\n";
    let s = run(source, true).unwrap();
    assert_eq!(s.tier, "review");
    assert!(s.notes[0].contains("'b' is only read conditionally"));
}

#[test]
fn test_should_not_flag_a_column_also_read_unconditionally() {
    let source = "df = pd.read_json(\"x\")\nif flag:\n    print(df[\"a\"])\nprint(df[\"a\"])\n";
    assert_eq!(run(source, true).unwrap().tier, "safe");
}

#[test]
fn test_should_mark_a_module_level_variable_as_review() {
    let source = "df = pd.read_json(\"x\")\nprint(df[\"a\"])\n";
    let s = run(source, false).unwrap();
    assert_eq!(s.tier, "review");
    assert!(s.notes[0].contains("module-level"));
}

#[test]
fn test_should_escape_a_column_name_that_needs_it() {
    let source = "df = pd.read_json(\"x\")\nprint(df[\"a\\\"b\"])\n";
    assert!(run(source, true).unwrap().diff.contains(r#"df[["a\"b"]]"#));
}

#[test]
fn test_should_render_an_empty_diff_when_nothing_changed() {
    assert_eq!(render_unified_diff("f.py", "a\nb\n", "a\nb\n"), "");
}

#[test]
fn test_should_render_a_unified_diff_with_context() {
    let old = "1\n2\n3\n4\n5\n6\n7\n8\n";
    let new = "1\n2\n3\n4\nX\n5\n6\n7\n8\n";
    let diff = render_unified_diff("f.py", old, new);
    assert!(diff.starts_with("--- a/f.py\n+++ b/f.py\n@@"), "{diff}");
    assert!(diff.contains("+X\n"));
    assert!(diff.contains(" 4\n") && diff.contains(" 7\n"));
}

#[test]
fn test_should_render_a_diff_at_the_start_and_end_of_a_file() {
    let at_start = render_unified_diff("f.py", "a\nb\n", "X\na\nb\n");
    assert!(
        at_start.contains("+X\n") && at_start.contains("@@ -1,2 +1,3 @@"),
        "{at_start}"
    );
    let at_end = render_unified_diff("f.py", "a\nb\n", "a\nb\nX\n");
    assert!(
        at_end.contains("+X\n") && at_end.contains("@@ -1,2 +1,3 @@"),
        "{at_end}"
    );
}

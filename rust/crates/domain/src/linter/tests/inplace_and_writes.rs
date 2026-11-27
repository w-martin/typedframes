use super::*;

#[test]
fn test_should_apply_an_inplace_drop_of_a_literal_column() {
    // arrange / act
    let (linter, errors) = lint_for_leg_tests(
        "df.drop(columns=[\"a\"], inplace=True)\nprint(df[\"a\"])\nprint(df[\"b\"])\n",
    );

    // assert: `a` is gone, `b` remains, and the edit is its own leg.
    assert_eq!(linter.dataframes_total, 2);
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
    assert!(errors[0].message.contains("'a'"));
}

#[test]
fn test_should_treat_an_inplace_row_drop_as_leaving_columns_alone() {
    // arrange / act
    let (linter, errors) = lint_for_leg_tests("df.drop([0, 1], inplace=True)\nprint(df[\"a\"])\n");

    // assert
    assert_eq!(linter.dataframes_total, 1);
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_apply_an_inplace_rename_of_literal_columns() {
    // arrange / act
    let (_, errors) = lint_for_leg_tests(
        "df.rename(columns={\"a\": \"z\"}, inplace=True)\nprint(df[\"z\"])\nprint(df[\"a\"])\n",
    );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'a'"));
}

#[test]
fn test_should_leave_columns_alone_for_an_inplace_method_that_keeps_them() {
    // arrange / act
    let (linter, errors) = lint_for_leg_tests(
        "df.dropna(inplace=True)\ndf.sort_values(\"a\", inplace=True)\nprint(df[\"a\"])\n",
    );

    // assert
    assert_eq!(linter.dataframes_total, 1);
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_mark_unresolved_after_an_unmodelled_inplace_method() {
    // arrange / act
    let (linter, errors) =
        lint_for_leg_tests("df.some_unknown(inplace=True)\nprint(df[\"zzz\"])\n");

    // assert
    assert_eq!(linter.dataframes_total, 2);
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
    assert_eq!(linter.leg_events.len(), 1);
    assert!(linter.leg_events[0].reason.contains("inplace=True"));
}

#[test]
fn test_should_mark_unresolved_after_an_inplace_drop_with_a_dynamic_list() {
    // arrange / act
    let (_, errors) =
        lint_for_leg_tests("df.drop(columns=to_drop, inplace=True)\nprint(df[\"zzz\"])\n");

    // assert
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
}

#[test]
fn test_should_ignore_a_method_call_with_inplace_false() {
    // arrange / act
    let (linter, errors) = lint_for_leg_tests("df.some_unknown(inplace=False)\nprint(df[\"a\"])\n");

    // assert
    assert_eq!(linter.dataframes_total, 1);
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_replace_columns_with_a_literal_list_assignment() {
    // arrange / act
    let (_, errors) =
        lint_for_leg_tests("df.columns = [\"x\", \"y\"]\nprint(df[\"x\"])\nprint(df[\"a\"])\n");

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'a'"));
}

#[test]
fn test_should_mark_unresolved_after_a_dynamic_columns_assignment() {
    // arrange / act
    let (linter, errors) = lint_for_leg_tests("df.columns = new_names\nprint(df[\"zzz\"])\n");

    // assert
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
    assert!(linter.leg_events[0].reason.contains("`.columns`"));
}

#[test]
fn test_should_keep_the_case_fold_columns_assignment_unchanged() {
    // arrange / act
    let (_, errors) = lint_for_leg_tests("df.columns = df.columns.str.upper()\nprint(df[\"A\"])\n");

    // assert
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_add_columns_written_through_a_list_subscript() {
    // arrange / act
    let (linter, errors) =
        lint_for_leg_tests("df[[\"c\", \"d\"]] = other\nprint(df[\"c\"])\nprint(df[\"d\"])\n");

    // assert: the write adds both columns, so neither the write nor the reads are
    // errors, and the write is a single new leg.
    assert_eq!(linter.dataframes_total, 2);
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_add_columns_written_through_loc() {
    // arrange / act
    let (_, errors) =
        lint_for_leg_tests("df.loc[df[\"a\"] > 1, \"flag\"] = True\nprint(df[\"flag\"])\n");

    // assert: writing a new column to an inferred frame adds it.
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_not_report_a_list_subscript_write_to_existing_columns() {
    // arrange / act
    let (linter, errors) = lint_for_leg_tests("df[[\"a\", \"b\"]] = other\ndf.loc[:, \"a\"] = 0\n");

    // assert
    assert_eq!(linter.dataframes_total, 1);
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_report_an_unresolved_frame_once_across_writes_and_reads() {
    // arrange / act
    let (_, errors) = lint_for_leg_tests(
            "df = df.some_unknown()\ndf[[\"c\", \"d\"]] = other\ndf[\"e\"] = 1\nprint(df[\"c\"], df[\"e\"])\nprint(df.d)\n",
        );

    // assert: one report, on the first read, carrying the fix
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
    assert!(errors[0].message.contains("fix:"), "{}", errors[0].message);
    assert_eq!(errors[0].line, 6);
}

#[test]
fn test_should_add_a_new_column_to_an_inferred_frame_on_write() {
    // arrange / act
    let (_, errors) = lint_for_leg_tests(
            "df[\"new\"] = df[\"a\"] * 2\ndf[[\"p\", \"q\"]] = 0\nprint(df[\"new\"], df[\"p\"])\nprint(df[\"nope\"])\n",
        );

    // assert: only the column nothing wrote or loaded is an error
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
    assert!(errors[0].message.contains("'nope'"));
}

#[test]
fn test_should_add_a_new_column_to_a_dict_literal_frame_on_write() {
    // arrange / act
    let (_, errors) = lint_source(
        "import pandas as pd\ndf = pd.DataFrame({\"a\": [1]})\ndf[\"x\"] = 1\nprint(df[\"x\"])\n",
    );

    // assert
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_not_report_missing_column_for_an_unresolved_frame_passed_to_a_helper() {
    // arrange / act: the write targets an unresolved frame, so it is unverifiable;
    // the frame it leaves behind has no column list, which is not "no columns".
    let (_, errors) = lint_for_leg_tests(
        "def need(f):\n    return f[\"c\"]\ndf = df.some_unknown()\ndf[\"new\"] = need(df)\n",
    );

    // assert
    assert!(
        errors.iter().all(|e| e.code != CODE_MISSING_COLUMN),
        "errors: {errors:?}"
    );
}

#[test]
fn test_should_untrack_a_frame_that_is_deleted() {
    // arrange / act
    let (linter, errors) = lint_for_leg_tests("del df\nprint(df[\"zzz\"])\n");

    // assert
    assert!(errors.is_empty(), "errors: {errors:?}");
    assert!(linter.leg_events[0].reason.contains("`del`"));
}

#[test]
fn test_should_untrack_a_frame_rebound_as_a_loop_variable() {
    // arrange / act
    let (linter, errors) =
        lint_for_leg_tests("for df in frames:\n    print(df[\"zzz\"])\nprint(df[\"zzz\"])\n");

    // assert
    assert!(errors.is_empty(), "errors: {errors:?}");
    assert!(linter.leg_events[0].reason.contains("loop variable"));
}

#[test]
fn test_should_untrack_a_frame_rebound_by_with() {
    // arrange / act
    let (linter, errors) = lint_for_leg_tests("with opener() as df:\n    print(df[\"zzz\"])\n");

    // assert
    assert!(errors.is_empty(), "errors: {errors:?}");
    assert!(linter.leg_events[0].reason.contains("`with ... as`"));
}

#[test]
fn test_should_untrack_a_frame_rebound_by_a_walrus() {
    // arrange / act
    let (linter, errors) =
        lint_for_leg_tests("if (df := fetch()) is not None:\n    print(df[\"zzz\"])\n");

    // assert
    assert!(errors.is_empty(), "errors: {errors:?}");
    assert!(linter.leg_events[0]
        .reason
        .contains("assignment expression"));
}

#[test]
fn test_should_not_untrack_a_module_frame_rebound_inside_a_function() {
    // arrange / act
    let (_, errors) = lint_for_leg_tests(
        "def f():\n    for df in frames:\n        pass\n\n\nprint(df[\"zzz\"])\n",
    );

    // assert: the module-level `df` is still validated.
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
}

#[test]
fn test_should_leave_unrelated_names_alone_on_rebinding_forms() {
    // arrange / act
    let (linter, errors) = lint_for_leg_tests(
            "for other in frames:\n    pass\ndel other\nwith opener() as fh:\n    pass\nprint(df[\"a\"])\n",
        );

    // assert
    assert!(errors.is_empty(), "errors: {errors:?}");
    assert!(linter.leg_events.is_empty());
}

#[test]
fn test_should_not_treat_a_positional_inplace_rename_as_a_column_rename() {
    // arrange / act: in pandas a positional mapper renames row labels.
    let (linter, errors) =
        lint_for_leg_tests("df.rename({\"a\": \"x\"}, inplace=True)\nprint(df[\"a\"])\n");

    // assert
    assert_eq!(linter.dataframes_total, 1);
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_rename_columns_for_a_positional_mapper_with_axis_one() {
    // arrange / act
    let (_, errors) =
        lint_for_leg_tests("df.rename({\"a\": \"x\"}, axis=1, inplace=True)\nprint(df[\"a\"])\n");

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'a'"));
}

#[test]
fn test_should_mark_unresolved_for_an_inplace_rename_with_a_dynamic_mapper() {
    // arrange / act
    let (_, errors) =
        lint_for_leg_tests("df.rename(columns=str.lower, inplace=True)\nprint(df[\"zzz\"])\n");

    // assert
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
}

#[test]
fn test_should_trace_an_inplace_flag_bound_once_to_true() {
    // arrange / act
    let (_, errors) = lint_for_leg_tests(
        "flag = True\ndf.drop(columns=[\"a\"], inplace=flag)\nprint(df[\"a\"])\n",
    );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
}

#[test]
fn test_should_trace_an_inplace_flag_bound_once_to_false() {
    // arrange / act
    let (linter, errors) = lint_for_leg_tests(
        "flag = False\ndf.drop(columns=[\"a\"], inplace=flag)\nprint(df[\"a\"])\n",
    );

    // assert
    assert_eq!(linter.dataframes_total, 1);
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_assume_not_in_place_for_an_untraceable_inplace_flag() {
    // arrange / act: `flag` is bound twice, so its value is unknown.
    let (linter, errors) = lint_for_leg_tests(
            "flag = True\nflag = compute()\ndf.drop(columns=[\"a\"], inplace=flag)\nprint(df[\"a\"])\ndf.drop(columns=[\"a\"], inplace=other())\n",
        );

    // assert
    assert_eq!(linter.dataframes_total, 1);
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_check_column_reads_inside_a_loc_mask() {
    // arrange / act
    let (_, errors) = lint_for_leg_tests("df.loc[df[\"zz\"] > 1, \"flag\"] = True\n");

    // assert: only the misspelled mask column; the written column is added.
    let messages: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
    assert_eq!(errors.len(), 1, "errors: {messages:?}");
    assert!(messages[0].contains("'zz'"));
}

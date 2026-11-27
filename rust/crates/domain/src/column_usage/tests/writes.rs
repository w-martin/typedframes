use super::*;

#[test]
fn test_should_track_a_walrus_assignment_target() {
    // arrange / act: `y := df` binds `y` in the ENCLOSING scope, unlike a
    // lambda/comprehension -- must be tracked the same as a plain `y = df`.
    let u = usage("if (y := df):\n    pass\nprint(y[\"a\"])\n");

    // assert
    assert_eq!(cols(&u), vec![("a", false)]);
}

#[test]
fn test_should_not_double_report_a_walrus_pure_alias() {
    // arrange / act: regression test, the `Expr::Named` counterpart of
    // `test_should_follow_a_variable_derived_from_the_root_via_reassignment` --
    // a pure walrus alias must not ALSO register as a generic "referenced" use.
    let u = usage("y = (z := df)\n");

    // assert
    assert!(u.other_uses.is_empty());
}

#[test]
fn test_should_not_double_report_an_annassign_pure_alias() {
    // arrange / act: the `Stmt::AnnAssign` counterpart of
    // `test_should_follow_a_variable_derived_from_the_root_via_reassignment` --
    // same guard, previously untested for this specific statement form.
    let u = usage("out: object = df\nprint(out[\"a\"])\n");

    // assert
    assert_eq!(cols(&u), vec![("a", false)]);
    assert!(u.other_uses.is_empty());
}

#[test]
fn test_should_not_double_report_a_self_referencing_boolean_mask() {
    // arrange / act: regression test -- `df[df > 0]` is a real, documented
    // whole-frame boolean-mask idiom. The tainted `Expr::Subscript` branch
    // already records ONE observation for the whole operation ("this
    // subscript"); the slice must not ALSO be walked generically afterward, or
    // the bare `df` inside the mask gets counted as a SECOND, separate use of
    // the same logical operation.
    let u = usage("print(df[df > 0])\n");

    // assert
    assert_eq!(
        others(&u),
        vec![("this subscript", false, Consumption::Unknown)]
    );
}

#[test]
fn test_should_not_double_report_a_self_referencing_attribute_mask() {
    // arrange / act: same root cause, via the `Expr::Attribute` arm instead --
    // `df[df.values]` must not record both "this subscript" and `.values`.
    let u = usage("print(df[df.values])\n");

    // assert
    assert_eq!(
        others(&u),
        vec![("this subscript", false, Consumption::Unknown)]
    );
}

#[test]
fn test_should_still_record_a_real_column_access_in_a_mask_built_from_one_column() {
    // arrange / act: the much more common "mask built from one column" idiom,
    // `df[df["age"] > 30]`, must NOT be affected by the self-mask fix above --
    // the inner occurrence is a proper Subscript (a real access), not a bare
    // Name, so it was never part of the double-count in the first place.
    let u = usage("print(df[df[\"age\"] > 30])\n");

    // assert
    assert_eq!(cols(&u), vec![("age", false)]);
    assert_eq!(
        others(&u),
        vec![("this subscript", false, Consumption::Unknown)]
    );
}

#[test]
fn test_should_follow_a_frame_filtered_by_a_mask_of_its_own_columns() {
    for source in [
        "out = df[df[\"a\"] > 0]\nprint(out[\"b\"])\n",
        "out = df[~df[\"a\"].isin(keep)].reset_index(drop=True)\nprint(out[\"b\"])\n",
        "out = df[df[\"a\"].notna() & (df[\"c\"] == 1)]\nprint(out[\"b\"])\n",
        "out = df[df[\"a\"].str.startswith(\"x\")].copy()\nprint(out[\"b\"])\n",
        "out = df.loc[df[\"a\"] > 0]\nprint(out[\"b\"])\n",
        "out = df.loc[df[\"a\"] > 0, :]\nprint(out[\"b\"])\n",
    ] {
        // act
        let u = usage(source);

        // assert
        assert!(u.other_uses.is_empty(), "{source}: {:?}", u.other_uses);
        let read: HashSet<&str> = u.accesses.iter().map(|a| a.column.as_str()).collect();
        assert!(
            read.contains("a") && read.contains("b"),
            "{source}: {read:?}"
        );
    }
}

#[test]
fn test_should_follow_a_row_slice() {
    // act
    let u = usage("out = df[:10]\nprint(out[\"b\"])\n");

    // assert
    assert!(u.other_uses.is_empty(), "{:?}", u.other_uses);
    assert_eq!(cols(&u), vec![("b", false)]);
}

#[test]
fn test_should_read_the_columns_a_loc_or_filtered_frame_selects() {
    for (source, expected) in [
        ("print(df.loc[df[\"a\"] > 0, \"b\"])\n", vec!["a", "b"]),
        ("print(df.loc[:, \"b\"])\n", vec!["b"]),
        (
            "print(df.loc[df[\"a\"] == 0, [\"b\", \"c\"]])\n",
            vec!["a", "b", "c"],
        ),
        ("print(df[df[\"a\"] > 0][\"b\"])\n", vec!["a", "b"]),
    ] {
        // act
        let u = usage(source);

        // assert
        assert!(u.other_uses.is_empty(), "{source}: {:?}", u.other_uses);
        let mut read: Vec<&str> = u.accesses.iter().map(|a| a.column.as_str()).collect();
        read.sort_unstable();
        assert_eq!(read, expected, "{source}");
    }
}

#[test]
fn test_should_not_take_a_selector_that_may_name_columns_as_a_row_mask() {
    for source in [
        "out = df[keep]\n",
        "out = df[other[\"a\"] > 0]\n",
        "out = df[0 < df[\"a\"] < 5]\n",
        "out = df.loc[keep, \"b\"]\n",
        "out = df.loc[df[\"a\"] > 0, cols]\n",
        "out = df[df[\"a\"] > 0].reset_index()\n",
    ] {
        // act
        let u = usage(source);

        // assert
        assert!(!u.other_uses.is_empty(), "{source}");
    }
}

#[test]
fn test_should_keep_a_filtered_frame_that_escapes_as_an_unknown_use() {
    // act
    let u = usage(
        "print(df[df[\"a\"] > 0])\nreturn_value = df.loc[df[\"a\"] > 0]\nfoo(return_value)\n",
    );

    // assert
    assert!(has_other(&u, "this subscript"), "{:?}", u.other_uses);
    assert!(has_other(&u, "passed to `foo()`"), "{:?}", u.other_uses);
}

#[test]
fn test_should_not_record_a_column_write_as_an_access() {
    // arrange / act: `df["new"] = ...` creates/overwrites a column; recording it
    // as a read would make a column that may not exist in the source look required.
    let u = usage("df[\"new\"] = 1\n");

    // assert
    assert!(u.accesses.is_empty());
    assert!(u.other_uses.is_empty());
}

#[test]
fn test_should_still_record_the_reads_on_the_right_of_a_column_write() {
    let u = usage("df[\"b\"] = df[\"a\"] * 2\n");
    assert_eq!(cols(&u), vec![("a", false)]);
}

#[test]
fn test_should_not_record_a_read_of_a_column_the_scope_wrote_first() {
    let u = usage("df[\"t\"] = df[\"a\"] * 2\nprint(df[\"t\"], df[\"b\"])\n");
    assert_eq!(cols(&u), vec![("a", false), ("b", false)]);
    assert!(u.wrote_columns);
}

#[test]
fn test_should_not_treat_a_conditional_write_as_creating_the_column() {
    let u = usage("if flag:\n    df[\"c\"] = 1\nprint(df[\"c\"])\n");
    assert_eq!(cols(&u), vec![("c", false)]);
    assert!(u.wrote_columns);
}

#[test]
fn test_should_read_a_column_before_it_is_written_as_a_source_read() {
    let u = usage("print(df[\"x\"])\ndf[\"x\"] = 1\nprint(df[\"x\"])\n");
    assert_eq!(cols(&u), vec![("x", false)]);
}

#[test]
fn test_should_count_a_list_assignment_as_writing_each_column() {
    let u = usage("df[[\"p\", \"q\"]] = f()\nprint(df[\"p\"], df[\"q\"])\n");
    assert!(cols(&u).is_empty());
}

#[test]
fn test_should_record_an_augmented_assignment_on_a_column_as_a_read() {
    // `df["b"] += 1` reads `b` before it writes it.
    let u = usage("df[\"b\"] += 1\n");
    assert_eq!(cols(&u), vec![("b", false)]);
}

#[test]
fn test_should_record_a_masked_assignment_as_unclassified() {
    let u = usage("df[df[\"a\"] > 1] = 0\n");
    assert!(has_other(&u, "assignment through this subscript"));
    assert_eq!(cols(&u), vec![("a", false)]);
}

#[test]
fn test_should_record_an_attribute_assignment_on_the_frame() {
    // `df.columns = [...]` renames every column.
    let u = usage("df.columns = [\"x\", \"y\"]\n");
    assert!(has_other(&u, "`.columns` assigned"));
}

#[test]
fn test_should_record_the_frame_escaping_into_an_attribute() {
    let u = usage("self.df = df\n");
    assert_eq!(
        others(&u),
        vec![(
            "stored in an attribute or container",
            false,
            Consumption::Unknown
        )]
    );
}

#[test]
fn test_should_record_the_frame_escaping_into_a_container() {
    let u = usage("cache[\"k\"] = df\n");
    assert!(has_other(&u, "stored in an attribute or container"));
}

#[test]
fn test_should_visit_a_for_else_body() {
    let u = usage("for x in xs:\n    pass\nelse:\n    print(df[\"a\"])\n");
    assert_eq!(cols(&u), vec![("a", true)]);
}

#[test]
fn test_should_visit_a_while_else_body() {
    let u = usage("while flag:\n    pass\nelse:\n    print(df[\"a\"])\n");
    assert_eq!(cols(&u), vec![("a", true)]);
}

#[test]
fn test_should_keep_the_taint_when_a_conditional_reassignment_may_not_run() {
    // arrange / act: after the `if`, `df` may still be the ORIGINAL frame, so the
    // later read must still count -- and the reassignment must be flagged.
    let u = usage("if flag:\n    df = other()\nprint(df[\"a\"])\n");

    // assert
    assert_eq!(cols(&u), vec![("a", false)]);
    assert!(has_other(&u, "`df` conditionally reassigned"));
}

#[test]
fn test_should_flag_a_loop_variable_that_rebinds_the_frame() {
    let u = usage("for df in frames:\n    pass\n");
    assert!(has_other(
        &u,
        "`df` rebound by a loop, `with` or `except` target"
    ));
}

#[test]
fn test_should_flag_a_with_target_that_rebinds_the_frame() {
    let u = usage("with open(p) as df:\n    pass\n");
    assert!(has_other(
        &u,
        "`df` rebound by a loop, `with` or `except` target"
    ));
}

#[test]
fn test_should_flag_an_except_target_that_rebinds_the_frame() {
    let u = usage("try:\n    pass\nexcept ValueError as df:\n    pass\n");
    assert!(has_other(
        &u,
        "`df` rebound by a loop, `with` or `except` target"
    ));
}

#[test]
fn test_should_not_flag_a_loop_variable_unrelated_to_the_frame() {
    let u = usage("for row in rows:\n    pass\n");
    assert!(u.other_uses.is_empty());
}

#[test]
fn test_should_visit_decorators_and_parameter_defaults_of_a_nested_def() {
    // arrange / act: both are evaluated in THIS scope when the `def` runs.
    let u = usage("@cache(df[\"a\"])\ndef f(x=df[\"b\"]):\n    pass\n");

    // assert
    assert_eq!(cols(&u), vec![("a", false), ("b", false)]);
}

#[test]
fn test_should_visit_class_bases() {
    let u = usage("class C(make(df)):\n    pass\n");
    assert!(has_other(&u, "passed to `make()`"));
}

#[test]
fn test_should_visit_a_lambda_default() {
    let u = usage("f = lambda x=df[\"a\"]: x\n");
    assert_eq!(cols(&u), vec![("a", false)]);
}

#[test]
fn test_should_flag_dynamic_name_lookups() {
    for call in ["eval(s)", "exec(s)", "locals()", "globals()", "vars()"] {
        let u = usage(&format!("{call}\n"));
        assert!(
            u.other_uses
                .iter()
                .any(|o| o.description.ends_with("can reach any variable")),
            "{call}"
        );
    }
}

#[test]
fn test_should_visit_a_nested_fstring_format_spec() {
    let u = usage("msg = f\"{x:{df}}\"\n");
    assert!(has_other(&u, "referenced"));
}

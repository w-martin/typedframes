use super::*;

#[test]
fn test_should_withhold_for_row_operations_that_compare_every_column() {
    for call in [
        "drop_duplicates()",
        "dropna()",
        "duplicated()",
        "unique()",
        "drop_nulls()",
        "drop_duplicates(keep=\"last\")",
        "copy().drop_duplicates()",
        "drop_duplicates().copy()",
    ] {
        let u = usage(&format!("x = df.{call}\nprint(x[\"a\"])\n"));
        assert!(
            u.other_uses.iter().any(|o| blocks_narrowing(o)
                && (call.contains("copy") || o.description.contains("considers every column"))),
            "{call}: {u:?}"
        );
    }
}

#[test]
fn test_should_follow_row_operations_with_a_literal_subset() {
    for call in [
        "drop_duplicates(subset=[\"id\"])",
        "dropna(subset=[\"id\"])",
        "unique(subset=\"id\")",
        "drop_nulls(\"id\")",
    ] {
        let u = usage(&format!("x = df.{call}\nprint(x[\"a\"])\n"));
        assert_eq!(cols(&u), vec![("id", false), ("a", false)], "{call}");
        assert!(u.other_uses.is_empty(), "{call}: {u:?}");
    }
}

#[test]
fn test_should_follow_a_frame_derived_without_column_arguments() {
    let u = usage("x = df.sort_index().copy()\nprint(x[\"a\"])\ny = x.head(5)\nprint(y[\"b\"])\n");
    assert_eq!(cols(&u), vec![("a", false), ("b", false)]);
    assert!(u.other_uses.is_empty(), "{u:?}");
}

#[test]
fn test_should_not_follow_a_derivation_whose_argument_names_a_column() {
    let u = usage("x = df.drop_duplicates(keys)\nprint(x[\"a\"])\n");
    assert!(u.other_uses.iter().any(blocks_narrowing), "{u:?}");
}

#[test]
fn test_should_follow_a_row_operation_with_literal_key_columns_and_read_them() {
    let u = usage(
            "x = df.drop_duplicates(subset=[\"id\"]).sort_values(\"t\", ascending=False)\nprint(x[\"a\"])\n",
        );
    assert_eq!(cols(&u), vec![("id", false), ("t", false), ("a", false)]);
    assert!(u.other_uses.is_empty(), "{u:?}");
}

#[test]
fn test_should_follow_a_frame_derived_by_with_columns_and_not_count_new_columns() {
    let u = usage("x = df.with_columns((pl.col(\"a\") * 2).alias(\"d\"), e=pl.col(\"b\"))\nprint(x[\"d\"], x[\"e\"], x[\"c\"])\n");
    assert_eq!(cols(&u), vec![("a", false), ("b", false), ("c", false)]);
    assert!(u.other_uses.is_empty(), "{u:?}");
}

#[test]
fn test_should_not_follow_a_with_columns_whose_output_name_is_unreadable() {
    let u = usage("x = df.with_columns(pl.all() * 2)\nprint(x[\"c\"])\n");
    assert!(u.other_uses.iter().any(blocks_narrowing), "{u:?}");
}

#[test]
fn test_should_record_an_unconditional_column_access() {
    let u = usage("print(df[\"a\"])\nprint(df[\"b\"])\n");
    assert_eq!(cols(&u), vec![("a", false), ("b", false)]);
    assert!(u.other_uses.is_empty());
}

#[test]
fn test_should_record_a_list_subscript_as_multiple_accesses() {
    let u = usage("print(df[[\"a\", \"b\"]])\n");
    assert_eq!(cols(&u), vec![("a", false), ("b", false)]);
}

#[test]
fn test_should_mark_an_access_inside_a_branch_as_conditional() {
    let u = usage("if flag:\n    print(df[\"a\"])\nprint(df[\"b\"])\n");
    assert_eq!(cols(&u), vec![("a", true), ("b", false)]);
}

#[test]
fn test_should_mark_accesses_inside_for_while_with_try_as_conditional() {
    let u = usage(
            "for x in xs:\n    print(df[\"a\"])\nwhile flag:\n    print(df[\"b\"])\nwith open(p) as f:\n    print(df[\"c\"])\ntry:\n    print(df[\"d\"])\nexcept Exception:\n    print(df[\"e\"])\n",
        );
    assert_eq!(
        cols(&u),
        vec![
            ("a", true),
            ("b", true),
            ("c", true),
            ("d", true),
            ("e", true)
        ]
    );
}

#[test]
fn test_should_follow_a_variable_derived_from_the_root_via_reassignment() {
    let u = usage("out = df\nprint(out[\"a\"])\n");
    assert_eq!(cols(&u), vec![("a", false)]);
    // A pure alias must not ALSO register as a generic "referenced" use of the
    // RHS `df` -- it's fully captured by becoming tainted, not an unrecognized
    // use (regression test: `visit_expr`'s bare-Name fallback must not fire here).
    assert!(u.other_uses.is_empty());
}

#[test]
fn test_should_follow_a_variable_derived_via_a_call_forwarding_the_root() {
    let u = usage("out = clean(df)\nprint(out[\"a\"])\n");
    assert_eq!(cols(&u), vec![("a", false)]);
}

#[test]
fn test_should_not_follow_an_unrelated_variable() {
    let u = usage("other = something_else()\nprint(other[\"a\"])\n");
    assert!(u.accesses.is_empty());
}

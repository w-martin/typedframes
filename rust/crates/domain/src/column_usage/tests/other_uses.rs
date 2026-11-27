use super::*;

#[test]
fn test_should_record_a_method_call_as_an_other_use_not_an_access() {
    let u = usage("df.to_dict()\n");
    assert_eq!(others(&u), vec![("`.to_dict()`", false, Consumption::All)]);
    assert!(u.accesses.is_empty());
}

#[test]
fn test_should_record_an_attribute_access_as_an_other_use() {
    let u = usage("names = list(df.columns)\n");
    assert_eq!(others(&u), vec![("`.columns`", false, Consumption::None)]);
}

#[test]
fn test_should_record_being_passed_to_a_call_as_an_other_use() {
    let u = usage("helper(df)\n");
    assert_eq!(
        others(&u),
        vec![("passed to `helper()`", false, Consumption::Unknown)]
    );
}

#[test]
fn test_should_record_being_returned_as_an_other_use() {
    let u = usage("def f():\n    return df\n");
    // `return` inside a nested function isn't walked (own scope) -- construct a
    // bare top-level return instead via a for-loop body to keep it in scope.
    assert!(u.other_uses.is_empty());
    let u2 = usage("if flag:\n    return df\n");
    assert_eq!(others(&u2), vec![("returned", true, Consumption::Unknown)]);
}

#[test]
fn test_should_record_a_direct_iteration_as_an_other_use() {
    // arrange / act: the iterable itself is evaluated once, unconditionally, the
    // moment the `for` statement runs -- the LOOP BODY is what's conditional
    // (depth only increments once entering it, checked separately below).
    let u = usage("for col in df:\n    print(col)\n");

    // assert
    assert_eq!(
        others(&u),
        vec![("iterated over directly", false, Consumption::Unknown)]
    );
}

#[test]
fn test_should_mark_the_for_loop_body_conditional_but_not_its_header() {
    // arrange / act
    let u = usage("for x in xs:\n    print(df[\"a\"])\n");

    // assert
    assert_eq!(cols(&u), vec![("a", true)]);
}

#[test]
fn test_should_record_an_unrecognized_subscript_as_an_other_use() {
    let u = usage("mask = df[\"a\"] > 1\nprint(df[mask])\n");
    // `df["a"]` in the mask expression is a real, unconditional access; the
    // boolean-mask subscript on the next line is an unrecognized shape.
    assert_eq!(cols(&u), vec![("a", false)]);
    assert_eq!(
        others(&u),
        vec![("this subscript", false, Consumption::Unknown)]
    );
}

#[test]
fn test_should_classify_an_unrecognized_method_name_as_unknown() {
    let u = usage("df.some_custom_transform()\n");
    assert_eq!(
        others(&u),
        vec![("`.some_custom_transform()`", false, Consumption::Unknown)]
    );
}

#[test]
fn test_should_classify_a_non_consuming_attribute_by_name() {
    let u = usage("n = df.shape[0]\n");
    assert_eq!(others(&u), vec![("`.shape`", false, Consumption::None)]);
}

#[test]
fn test_should_record_a_printed_shape_or_head_as_output_not_a_blocker() {
    let u = usage("print(df.shape)\nprint(df.head(3))\ndf.head()\n");
    assert!(u.other_uses.iter().all(|o| !blocks_narrowing(o)), "{u:?}");
    assert_eq!(u.other_uses.len(), 3);
}

#[test]
fn test_should_keep_a_shape_that_feeds_other_code_as_a_blocker() {
    let u = usage("x = np.zeros(df.shape)\ncols = [c for c in df.columns]\n");
    assert!(u.other_uses.iter().all(blocks_narrowing), "{u:?}");
}

#[test]
fn test_should_not_descend_into_a_nested_function_or_class() {
    let u = usage("def f():\n    print(df[\"a\"])\n\n\nclass C:\n    x = df[\"b\"]\n");
    assert!(u.accesses.is_empty());
    assert!(u.other_uses.is_empty());
}

#[test]
fn test_should_stop_growing_taint_from_an_unrelated_reassignment() {
    // `df = other()` doesn't forward the root, so `df` is no longer tainted from
    // that point -- matches `expr_forwards_tainted`'s own rule.
    let u = usage("df = other()\nprint(df[\"a\"])\n");
    assert!(u.accesses.is_empty());
}

#[test]
fn test_should_untaint_a_name_reassigned_away_from_the_origin() {
    // arrange / act: `out` picks up the taint from `df`, then loses it once
    // reassigned to something unrelated -- the access after that must not be
    // recorded as if it were still about the original origin.
    let u = usage("out = df\nout = other()\nprint(out[\"a\"])\n");

    // assert
    assert!(u.accesses.is_empty());
}

#[test]
fn test_should_keep_the_root_tainted_when_a_derived_alias_is_reassigned() {
    // arrange / act: only `out` loses taint here, not `df` itself.
    let u = usage("out = df\nout = other()\nprint(df[\"a\"])\n");

    // assert
    assert_eq!(cols(&u), vec![("a", false)]);
}

#[test]
fn test_should_record_a_bare_reference_nested_in_a_list_literal() {
    // arrange / act: regression test -- `pd.concat([df, other])` used to
    // produce ZERO observations (not even Unknown), since `Expr::List` visits
    // its elements generically with no taint check, and there was no
    // `Expr::Name` fallback at all for a bare reference reached that way.
    let u = usage("pd.concat([df, other])\n");

    // assert
    assert_eq!(
        others(&u),
        vec![("referenced", false, Consumption::Unknown)]
    );
}

#[test]
fn test_should_record_a_bare_reference_in_a_binop() {
    let u = usage("total = df + 1\n");
    assert_eq!(
        others(&u),
        vec![("referenced", false, Consumption::Unknown)]
    );
}

#[test]
fn test_should_record_a_bare_reference_in_an_augmented_assignment() {
    let u = usage("total += df\n");
    assert_eq!(
        others(&u),
        vec![("referenced", false, Consumption::Unknown)]
    );
}

#[test]
fn test_should_not_record_anything_for_an_unrelated_name() {
    // arrange / act: the new `Expr::Name` fallback must only fire for the
    // TAINTED name, not any bare name reached the same way.
    let u = usage("pd.concat([other, another])\n");

    // assert
    assert!(u.other_uses.is_empty());
}

#[test]
fn test_should_record_an_assert_expression_use() {
    let u = usage("assert df[\"a\"].notna().all()\n");
    assert_eq!(cols(&u), vec![("a", false)]);
}

#[test]
fn test_should_record_a_raise_expression_use() {
    let u = usage("raise ValueError(df)\n");
    assert_eq!(
        others(&u),
        vec![("passed to `ValueError()`", false, Consumption::Unknown)]
    );
}

#[test]
fn test_should_mark_a_match_case_body_as_conditional() {
    let u = usage("match x:\n    case 1:\n        print(df[\"a\"])\n");
    assert_eq!(cols(&u), vec![("a", true)]);
}

#[test]
fn test_should_record_a_dict_literal_value_as_a_use() {
    let u = usage("summary = {\"total\": df}\n");
    assert_eq!(
        others(&u),
        vec![("referenced", false, Consumption::Unknown)]
    );
}

#[test]
fn test_should_record_a_set_literal_element_as_a_use() {
    let u = usage("s = {df}\n");
    assert_eq!(
        others(&u),
        vec![("referenced", false, Consumption::Unknown)]
    );
}

#[test]
fn test_should_record_an_fstring_interpolation_as_a_use() {
    let u = usage("msg = f\"{df}\"\n");
    assert_eq!(
        others(&u),
        vec![("referenced", false, Consumption::Unknown)]
    );
}

#[test]
fn test_should_record_a_slice_bound_as_a_use() {
    let u = usage("out = other[df:]\n");
    assert_eq!(
        others(&u),
        vec![("referenced", false, Consumption::Unknown)]
    );
}

#[test]
fn test_should_record_a_yield_value_as_a_use() {
    // arrange / act: `yield` inside a nested `def` isn't walked (own scope,
    // same as `return` -- see `test_should_record_being_returned_as_an_other_use`)
    // -- construct a bare top-level yield instead, the same way that test does.
    let u = usage("if flag:\n    yield df\n");

    // assert
    assert_eq!(others(&u), vec![("referenced", true, Consumption::Unknown)]);
}

#[test]
fn test_should_not_descend_into_a_lambda_body() {
    // arrange / act: same "own scope" treatment as a nested `def`.
    let u = usage("f = lambda: df[\"a\"]\n");

    // assert
    assert!(u.accesses.is_empty());
    assert!(u.other_uses.is_empty());
}

#[test]
fn test_should_record_the_outer_generator_iterable_as_a_real_access() {
    // arrange / act: CPython evaluates the outermost `for` clause's iterable in
    // the ENCLOSING scope, before the generator's own scope exists -- this is a
    // real, unconditional access happening here, not inside a skipped scope.
    let u = usage("total = sum(x for x in df[\"a\"])\n");

    // assert
    assert_eq!(cols(&u), vec![("a", false)]);
}

#[test]
fn test_should_not_descend_into_a_generator_body() {
    // arrange / act: a generator runs when consumed, so only its outermost iterable is
    // evaluated here.
    let u = usage("out = (df[\"a\"] for x in xs)\n");

    // assert
    assert!(u.accesses.is_empty());
    assert!(u.other_uses.is_empty());
}

#[test]
fn test_should_walk_an_eager_comprehension_body_as_conditional() {
    for source in [
        "out = [df[\"a\"] for x in xs if df[\"b\"] is not None]\n",
        "out = {x: df[\"a\"] for x in xs for y in df[\"b\"]}\n",
        "out = {k: {j: df[\"a\"] for j in js} for k in ks if df[\"b\"].any()}\n",
    ] {
        // act
        let u = usage(source);

        // assert
        assert!(u.other_uses.is_empty(), "{source}: {:?}", u.other_uses);
        let mut read = cols(&u);
        read.sort_unstable();
        assert_eq!(read, vec![("a", true), ("b", true)], "{source}");
    }
}

#[test]
fn test_should_record_uses_inside_an_eager_comprehension() {
    // act
    let u = usage("out = [f(df, x) for x in xs]\nother = [df for df in frames]\n");

    // assert
    assert!(has_other(&u, "passed to `f()`"), "{:?}", u.other_uses);
    assert!(
        has_other(&u, "rebound by a comprehension target"),
        "{:?}",
        u.other_uses
    );
}

use super::*;

#[test]
fn test_should_record_the_line_of_each_observation() {
    // arrange
    let source = "x = 1\nprint(df[\"a\"])\n\ndf.to_dict()\n";
    let module = parse_module(source).unwrap().into_syntax();
    let line_of = |offset: TextSize| source[..offset.to_usize()].matches('\n').count() + 1;

    // act
    let u = scan_usage("df", &module.body, &line_of);

    // assert
    assert_eq!(u.accesses[0].line, 2);
    assert_eq!(u.other_uses[0].line, 4);
}

#[test]
fn test_should_track_every_alias_in_ever_tainted() {
    let u = usage("out = df\nout = other()\nalias = out\n");
    assert!(u.ever_tainted.contains("df"));
    assert!(u.ever_tainted.contains("out"));
    assert!(!u.ever_tainted.contains("alias"));
}

#[test]
fn test_should_find_a_mention_inside_a_nested_function() {
    assert!(mention("def f():\n    return df[\"a\"]\n"));
}

#[test]
fn test_should_find_a_mention_in_a_function_defined_before_the_origin() {
    // `body` is the whole scope: a function above the origin still runs later.
    assert!(mention("def f():\n    return df\ndf = load()\n"));
}

#[test]
fn test_should_find_a_mention_inside_a_lambda_body() {
    assert!(mention("f = lambda: df\n"));
}

#[test]
fn test_should_find_a_mention_inside_a_generator_body() {
    assert!(mention("out = (df[\"a\"] for x in xs)\n"));
    assert!(mention("out = (y for x in xs for y in df)\n"));
    assert!(mention("out = [(df[\"a\"] for _ in ys) for x in xs]\n"));
    assert!(mention("out = [lambda: df for x in xs]\n"));
}

#[test]
fn test_should_not_count_an_eager_comprehension_body() {
    // it runs here, and `scan_usage` walks it.
    assert!(!mention("out = [df[\"a\"] for x in xs]\n"));
    assert!(!mention("out = {x: y for x in xs for y in df}\n"));
}

#[test]
fn test_should_find_a_mention_inside_a_class_body() {
    assert!(mention("class C:\n    x = df\n"));
}

#[test]
fn test_should_not_count_the_outermost_comprehension_iterable() {
    // evaluated in the enclosing scope -- `scan_usage` already records it.
    assert!(!mention("out = sum(x for x in df[\"a\"])\n"));
}

#[test]
fn test_should_not_count_decorators_defaults_or_class_bases() {
    assert!(!mention("@cache(df)\ndef f(x=df):\n    pass\n"));
    assert!(!mention("class C(make(df)):\n    pass\n"));
    assert!(!mention("f = lambda x=df: x\n"));
}

#[test]
fn test_should_not_find_a_mention_of_an_unrelated_name() {
    assert!(!mention("def f():\n    return other\n"));
}

#[test]
fn test_should_not_count_top_level_code() {
    assert!(!mention("print(df)\n"));
}

#[test]
fn test_should_find_a_global_or_nonlocal_declaration() {
    assert!(declares("global df\n"));
    assert!(declares("if flag:\n    nonlocal df\n"));
}

#[test]
fn test_should_ignore_a_declaration_of_another_name_or_in_a_nested_def() {
    assert!(!declares("global other\n"));
    assert!(!declares("def f():\n    global df\n"));
}

#[test]
fn test_should_name_a_module_attribute_callee() {
    let u = usage("np.log(df)\n");
    assert_eq!(
        others(&u),
        vec![("passed to `np.log()`", false, Consumption::Unknown)]
    );
}

#[test]
fn test_should_name_a_keyword_argument_callee_too() {
    let u = usage("fit(data=df)\n");
    assert_eq!(
        others(&u),
        vec![("passed to `fit()`", false, Consumption::Unknown)]
    );
}

#[test]
fn test_should_fall_back_to_a_generic_label_for_an_unnameable_callee() {
    let u = usage("registry[\"k\"](df)\n");
    assert_eq!(
        others(&u),
        vec![("passed to a call", false, Consumption::Unknown)]
    );
    let u = usage("a.b.c(df)\n");
    assert_eq!(
        others(&u),
        vec![("passed to a call", false, Consumption::Unknown)]
    );
}

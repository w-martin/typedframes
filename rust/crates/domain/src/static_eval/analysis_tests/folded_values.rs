use super::*;

#[test]
fn test_should_not_fold_a_cyclic_definition() {
    assert_eq!(used_as("A = B\nB = A\nprint(A)\n", "A"), None);
    assert_eq!(used_as("A = A + [1]\nprint(A)\n", "A"), None);
}

#[test]
fn test_should_not_fold_what_depends_on_runtime_state() {
    for rhs in [
        "open(p).read().split()",
        "json.print(f)",
        "os.environ[\"X\"].split(\",\")",
        "sys.argv[1:]",
        "random.sample([\"a\", \"b\"], 1)",
        "[x for x in get_cols()]",
        "list(df.columns)",
        "other_module.COLS",
        "param_default",
    ] {
        let source = format!("COLS = {rhs}\nprint(usecols=COLS)\n");
        assert_eq!(used_as(&source, "COLS"), None, "{rhs}");
    }
}

#[test]
fn test_should_fold_into_a_conditional_expression_and_a_comparison() {
    let source = "DEBUG = False\nCOLS = [\"a\"] if DEBUG else [\"b\"]\nprint(usecols=COLS)\n";
    assert_eq!(used_as(source, "COLS"), Some(strs(&["b"])));
}

#[test]
fn test_should_fold_a_dict_and_its_keys() {
    let source =
        "D = {\"a\": 1, \"b\": 2}\nprint(usecols=list(D))\nprint(usecols=sorted(D.keys()))\n";
    let folded = folds(source);
    assert!(folded
        .iter()
        .any(|(t, v)| t == "list(D)" && *v == strs(&["a", "b"])));
    assert!(folded
        .iter()
        .any(|(t, v)| t == "sorted(D.keys())" && *v == strs(&["a", "b"])));
}

#[test]
fn test_should_leave_a_use_inside_an_f_string_foldable() {
    let source = "TABLE = \"orders\"\nprint(sql=f\"select * from {TABLE}\")\n";
    assert!(folds(source)
        .iter()
        .any(|(t, v)| t == "f\"select * from {TABLE}\"" && *v == s("select * from orders")));
}

#[test]
fn test_should_replace_with_a_self_contained_literal_of_the_same_value() {
    // (setup, right-hand side, value it must equal once folded)
    let cases: Vec<(&str, &str, Value)> = vec![
        ("BASE = [\"a\"]", "BASE + [\"b\"]", strs(&["a", "b"])),
        (
            "BASE = (\"a\",)",
            "{\"k\": BASE}",
            Value::Dict(vec![(s("k"), Value::Tuple(vec![s("a")]))]),
        ),
        (
            "BASE = [\"a\"]",
            "(BASE[0], -3, True, None)",
            Value::Tuple(vec![s("a"), Value::Int(-3), Value::Bool(true), Value::None]),
        ),
        ("BASE = [\"a\"]", "{BASE[0]}", Value::Set(vec![s("a")])),
        ("BASE = [\"a\"]", "f\"x_{BASE[0]}\"", s("x_a")),
    ];
    for (setup, rhs, expected) in cases {
        let source = format!("{setup}\nOUT = {rhs}\n");
        assert_eq!(
            folded_value_of_last_statement(&source),
            Some(expected),
            "{rhs}"
        );
    }
}

#[test]
fn test_should_let_a_value_nothing_can_change_be_used_any_way_at_all() {
    // strings, ints and tuples of them can't be mutated through any alias
    for binding in [
        "NAME = \"orders\"",
        "NAME = 3",
        "NAME = (\"a\", 1)",
        "NAME = None",
    ] {
        for use_ in [
            "alias = NAME",
            "helper(NAME)",
            "other = [NAME]",
            "other = {\"k\": NAME}",
            "obj.attr = NAME",
            "x = NAME if flag else 0",
        ] {
            let source = format!("{binding}\n{use_}\nprint(NAME)\n");
            assert!(
                folds(&source).iter().any(|(t, _)| t == "NAME"),
                "{binding}: {use_}"
            );
        }
    }
}

#[test]
fn test_should_never_let_a_container_be_aliased_or_stored() {
    for binding in [
        "NAME = [\"a\"]",
        "NAME = {\"a\": 1}",
        "NAME = {\"a\"}",
        "NAME = ([\"a\"], 1)",
    ] {
        for use_ in [
            "alias = NAME",
            "helper(NAME)",
            "other = [NAME]",
            "other = {\"k\": NAME}",
        ] {
            let source = format!("{binding}\n{use_}\nprint(NAME)\n");
            assert!(
                !folds(&source).iter().any(|(t, _)| t == "NAME"),
                "{binding}: {use_}"
            );
        }
    }
}

#[test]
fn test_should_keep_the_original_source_range_of_a_folded_expression() {
    let source = "BASE = [\"a\"]\nOUT = BASE + [\"b\"]\n";
    let module = parse_module(source).unwrap().into_syntax();
    let folded = fold_constants(&module, &ImportedModules::new()).unwrap();
    let (ast::Stmt::Assign(original), ast::Stmt::Assign(new)) = (&module.body[1], &folded.body[1])
    else {
        panic!("expected assignments");
    };
    assert_eq!(original.value.range(), new.value.range());
    assert_eq!(
        new.value.range(),
        original.range().add_start(TextSize::from(6))
    );
}

#[test]
fn test_should_return_none_when_nothing_folds() {
    let module = parse_module("x = [\"a\"]\nprint(usecols=x2)\n")
        .unwrap()
        .into_syntax();
    assert!(fold_constants(&module, &ImportedModules::new()).is_none());
}

#[test]
fn test_should_not_fold_a_folded_literal_larger_than_the_cap() {
    let source = "BIG = list(range(2000))\nprint(x=BIG)\n";
    assert!(folds(source).iter().all(|(t, _)| t != "BIG"));
}

#[test]
fn test_should_not_fold_an_empty_set_which_has_no_literal_form() {
    assert!(folds("E = set()\nprint(x=E)\n")
        .iter()
        .all(|(t, _)| t != "E"));
}

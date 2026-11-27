use super::*;

#[test]
fn test_should_fold_a_module_constant_and_a_chain_of_them() {
    let source = "BASE = [\"a\"]\nCOLS = BASE + [\"b\"]\nprint(usecols=COLS)\n";
    assert_eq!(used_as(source, "COLS"), Some(strs(&["a", "b"])));
    // the right-hand side of COLS itself folds too
    assert!(folds(source)
        .iter()
        .any(|(t, v)| t == "BASE + [\"b\"]" && *v == strs(&["a", "b"])));
}

#[test]
fn test_should_fold_a_whole_expression_in_an_argument_position() {
    let source = "print(usecols=[f\"m{i}\" for i in range(2)])\n";
    assert_eq!(
        folds(source),
        vec![(
            "[f\"m{i}\" for i in range(2)]".to_string(),
            strs(&["m0", "m1"])
        )]
    );
}

#[test]
fn test_should_not_fold_a_plain_literal() {
    assert!(folds("print(usecols=[\"a\", \"b\"], n=3, d={\"k\": [1]})\n").is_empty());
}

#[test]
fn test_should_not_fold_assignment_targets_or_annotations() {
    assert!(folds("COLS = []\nCOLS[0] = 1\nx: COLS = 1\n").is_empty());
}

#[test]
fn test_should_resolve_each_function_local_name_in_its_own_scope() {
    let source = "def a():\n    cols = [\"x\"]\n    return print(usecols=cols)\n\n\ndef b():\n    cols = [\"y\"]\n    return print(usecols=cols)\n";
    let found: Vec<Value> = folds(source)
        .into_iter()
        .filter(|(t, _)| t == "cols")
        .map(|(_, v)| v)
        .collect();
    assert_eq!(found, vec![strs(&["x"]), strs(&["y"])]);
}

#[test]
fn test_should_see_a_module_constant_from_a_function_unless_it_is_shadowed() {
    let visible = "COLS = [\"m\"]\n\n\ndef f():\n    return print(usecols=COLS)\n";
    assert_eq!(used_as(visible, "COLS"), Some(strs(&["m"])));

    let shadowed =
        "COLS = [\"m\"]\n\n\ndef f():\n    COLS = [\"local\"]\n    return print(usecols=COLS)\n";
    let found: Vec<Value> = folds(shadowed)
        .into_iter()
        .filter(|(t, _)| t == "COLS")
        .map(|(_, v)| v)
        .collect();
    assert_eq!(found, vec![strs(&["local"])]);
}

#[test]
fn test_should_not_let_a_class_body_name_leak_into_its_methods() {
    let source =
        "class C:\n    COLS = [\"c\"]\n\n    def m(self):\n        return print(usecols=COLS)\n";
    assert_eq!(used_as(source, "COLS"), None);
}

#[test]
fn test_should_resolve_a_name_in_a_class_body_from_that_body() {
    let source = "class C:\n    COLS = [\"c\"]\n    other = print(usecols=COLS)\n";
    assert_eq!(used_as(source, "COLS"), Some(strs(&["c"])));
}

#[test]
fn test_should_resolve_through_a_closure() {
    let source = "def outer():\n    cols = [\"o\"]\n\n    def inner():\n        return print(usecols=cols)\n\n    return inner\n";
    assert_eq!(used_as(source, "cols"), Some(strs(&["o"])));
}

#[test]
fn test_should_respect_a_comprehension_variable_with_the_name_of_a_constant() {
    let source = "N = 5\nout = print(x=[N for N in [1, 2]])\n";
    assert_eq!(
        folds(source)
            .iter()
            .find(|(t, _)| t.starts_with("[N for"))
            .map(|(_, v)| v.clone()),
        Some(Value::List(vec![Value::Int(1), Value::Int(2)]))
    );
}

#[test]
fn test_should_not_fold_a_name_bound_more_than_once() {
    for extra in [
        "COLS = [\"b\"]",
        "COLS += [\"b\"]",
        "COLS, other = [\"b\"], 1",
        "for COLS in []:\n    pass",
        "with open(p) as COLS:\n    pass",
        "import COLS",
        "from m import COLS",
        "def COLS():\n    pass",
        "class COLS:\n    pass",
        "(COLS := [\"b\"])",
        "del COLS",
        "try:\n    pass\nexcept E as COLS:\n    pass",
        "match x:\n    case COLS:\n        pass",
        "match x:\n    case [*COLS]:\n        pass",
        "match x:\n    case {\"k\": 1, **COLS}:\n        pass",
    ] {
        let source = with_print(&format!("COLS = [\"a\"]\n{extra}"));
        assert_eq!(used_as(&source, "COLS"), None, "{extra}");
    }
}

#[test]
fn test_should_not_fold_a_parameter_or_a_comprehension_target() {
    assert_eq!(used_as("def f(COLS):\n    print(COLS)\n", "COLS"), None);
    assert_eq!(used_as("def f(*COLS):\n    print(COLS)\n", "COLS"), None);
    assert_eq!(used_as("lambda COLS: print(COLS)\n", "COLS"), None);
}

#[test]
fn test_should_not_fold_a_name_rebound_through_global_or_nonlocal() {
    let source =
        "COLS = [\"a\"]\n\n\ndef f():\n    global COLS\n    COLS = [\"b\"]\n\n\nprint(COLS)\n";
    assert_eq!(used_as(source, "COLS"), None);

    let source = "def outer():\n    cols = [\"a\"]\n\n    def inner():\n        nonlocal cols\n        cols = [\"b\"]\n\n    print(cols)\n";
    assert_eq!(used_as(source, "cols"), None);
}

#[test]
fn test_should_fold_a_name_a_function_only_reads_through_global() {
    let source = "COLS = [\"a\"]\n\n\ndef f():\n    global COLS\n    return print(usecols=COLS)\n";
    assert_eq!(used_as(source, "COLS"), Some(strs(&["a"])));
}

#[test]
fn test_should_not_fold_a_name_that_escapes() {
    for use_ in [
        "COLS.append(\"x\")",
        "COLS.extend([\"x\"])",
        "COLS.insert(0, \"x\")",
        "COLS.remove(\"a\")",
        "COLS.pop()",
        "COLS.clear()",
        "COLS.sort()",
        "COLS.reverse()",
        "COLS.update({})",
        "COLS.unknown_method()",
        "COLS[0] = \"x\"",
        "COLS[0:1] = []",
        "del COLS[0]",
        "COLS.attr = 1",
        "alias = COLS",
        "alias = [COLS]",
        "alias = (COLS, 1)",
        "alias = {\"k\": COLS}",
        "alias = {COLS}",
        "alias = COLS if flag else other",
        "alias = COLS or other",
        "alias = (y := COLS)",
        "helper(COLS)",
        "helper(x=COLS)",
        "helper(*COLS)",
        "obj.method(COLS)",
        "obj.attr = COLS",
        "obj[0] = COLS",
        "return_it = lambda: COLS",
        "with COLS:\n    pass",
        "raise COLS",
        "await COLS",
        "yield COLS",
        "COLS.__class__",
        "@COLS\ndef f():\n    pass",
    ] {
        let source = format!("COLS = [\"a\"]\nprint(usecols=COLS)\n{use_}\n");
        assert_eq!(used_as(&source, "COLS"), None, "{use_}");
    }
}

#[test]
fn test_should_not_fold_a_name_returned_or_stored_by_default_argument() {
    assert_eq!(
        used_as("COLS = [\"a\"]\n\n\ndef f():\n    return COLS\n", "COLS"),
        None
    );
    assert_eq!(
        used_as("COLS = [\"a\"]\n\n\ndef f(c=COLS):\n    return c\n", "COLS"),
        None
    );
}

#[test]
fn test_should_not_fold_a_name_mutated_from_inside_a_function() {
    let source = "COLS = [\"a\"]\n\n\ndef f():\n    COLS.append(\"x\")\n\n\nprint(usecols=COLS)\n";
    assert_eq!(used_as(source, "COLS"), None);
}

#[test]
fn test_should_fold_a_name_that_is_only_read() {
    for use_ in [
        "len(COLS)",
        "\"a\" in COLS",
        "if COLS:\n    pass",
        "print(COLS)",
        "COLS == other",
        "f\"{COLS}\"",
        "x = COLS[0]",
        "x = COLS[1:]",
        "for c in COLS:\n    pass",
        "x = [c for c in COLS]",
        "a, b = COLS",
        "x = COLS + [\"b\"]",
        "x = [*COLS, \"b\"]",
        "x = sorted(COLS)",
        "x = list(COLS)",
        "x = \",\".join(COLS)",
        "x = COLS.index(\"a\")",
        "x = COLS.count(\"a\")",
        "x = COLS.copy()",
        "pd.DataFrame(rows, columns=COLS)",
        "pd.read_csv(\"f\", usecols=COLS)",
    ] {
        let source = format!("COLS = [\"a\", \"b\"]\n{use_}\nprint(usecols=COLS)\n");
        assert_eq!(used_as(&source, "COLS"), Some(strs(&["a", "b"])), "{use_}");
    }
}

#[test]
fn test_should_let_mutable_elements_be_used_only_in_ways_that_expose_none_of_them() {
    // a dict of lists: `len` and a loader argument expose nothing ...
    for use_ in [
        "len(ROWS)",
        "pd.DataFrame(ROWS)",
        "\"x\" in ROWS",
        "print(ROWS)",
    ] {
        let source = format!("ROWS = {{\"x\": [1, 2]}}\n{use_}\nprint(data=ROWS)\n");
        assert!(used_as(&source, "ROWS").is_some(), "{use_}");
    }
    // ... but anything that hands out an inner list lets it be changed unseen
    for use_ in [
        "for k in ROWS:\n    pass",
        "x = ROWS[\"x\"]",
        "x = ROWS.values()",
        "x = ROWS.items()",
        "x = ROWS.get(\"x\")",
        "x = dict(ROWS)",
        "x = ROWS.copy()",
        "x = {**ROWS}",
        "x = [ROWS]",
        "x = [c for c in ROWS]",
    ] {
        let source = format!("ROWS = {{\"x\": [1, 2]}}\n{use_}\nprint(data=ROWS)\n");
        assert_eq!(used_as(&source, "ROWS"), None, "{use_}");
    }
}

#[test]
fn test_should_treat_a_list_of_dicts_the_same_way() {
    let flat = "ROWS = [{\"x\": 1}]\npd.DataFrame(ROWS)\nlen(ROWS)\n";
    assert!(folds(flat).iter().any(|(t, _)| t == "ROWS"));
    let exposed = "ROWS = [{\"x\": 1}]\nfirst = ROWS[0]\npd.DataFrame(ROWS)\n";
    assert!(!folds(exposed).iter().any(|(t, _)| t == "ROWS"));
}

#[test]
fn test_should_not_trust_a_builtin_the_file_rebinds() {
    // `len` is rebound, so `len(COLS)` may do anything to COLS
    let source =
        "COLS = [\"a\"]\n\n\ndef len(x):\n    x.append(1)\n\n\nlen(COLS)\nprint(usecols=COLS)\n";
    assert_eq!(used_as(source, "COLS"), None);
    // and a rebound builtin is not evaluated as the builtin: it is the file's own function
    let source = "def len(x):\n    return 99\n\n\nprint(n=len([1]))\n";
    assert_eq!(
        folds(source),
        vec![("len([1])".to_string(), Value::Int(99))]
    );
}

#[test]
fn test_should_not_trust_a_loader_name_the_file_defines_itself() {
    let own = "COLS = [\"a\"]\n\n\ndef read_csv(path, usecols):\n    usecols.append(1)\n\n\nread_csv(\"f\", usecols=COLS)\n";
    assert_eq!(used_as(own, "COLS"), None);
    let library = "COLS = [\"a\"]\nread_csv(\"f\", usecols=COLS)\n";
    assert_eq!(used_as(library, "COLS"), Some(strs(&["a"])));
}

#[test]
fn test_should_not_fold_module_names_when_the_namespace_can_change_unseen() {
    for hazard in [
        "globals()[\"COLS\"] = []",
        "vars()[\"COLS\"] = []",
        "exec(code)",
        "eval(code)",
        "locals()[\"COLS\"] = []",
        "from other import *",
    ] {
        let source = format!("COLS = [\"a\"]\n{hazard}\nprint(usecols=COLS)\n");
        assert_eq!(used_as(&source, "COLS"), None, "{hazard}");
    }
    // function locals are not affected by `globals()`
    let source = "def f():\n    cols = [\"a\"]\n    return print(usecols=cols)\n\n\nglobals()\n";
    assert_eq!(used_as(source, "cols"), Some(strs(&["a"])));
}

#[test]
fn test_should_fold_a_constant_with_a_plain_type_annotation() {
    for annotation in [
        "list[str]",
        "List[str]",
        "typing.List[str]",
        "Final",
        "Final[list[str]]",
        "Sequence[str]",
        "tuple[str, ...]",
        "Optional[list[str]]",
        "list[str] | None",
    ] {
        let source = format!("COLS: {annotation} = [\"a\"]\nprint(COLS)\n");
        assert_eq!(used_as(&source, "COLS"), Some(strs(&["a"])), "{annotation}");
    }
}

#[test]
fn test_should_not_fold_a_name_whose_annotation_says_it_is_something_else() {
    // `df` is a DataFrame to the checker even though its value here is `None`
    for annotation in [
        "Annotated[pl.DataFrame, Schema]",
        "pd.DataFrame",
        "DataFrame[Schema]",
        "Schema",
        "\"list[str]\"",
    ] {
        let source = format!("df: {annotation} = None\nprint(df)\n");
        assert_eq!(used_as(&source, "df"), None, "{annotation}");
    }
}

#[test]
fn test_should_not_replace_a_name_that_is_accessed_indexed_or_called() {
    // evaluating the whole expression fails (unknown method / key), and the name
    // itself must still be left alone
    for use_ in ["NAME.unknown()", "NAME.attr", "NAME[idx]", "NAME(1)"] {
        let source = format!("NAME = \"orders\"\nx = {use_}\n");
        assert!(folds(&source).iter().all(|(t, _)| t != "NAME"), "{use_}");
    }
}

#[test]
fn test_should_still_fold_a_name_used_as_an_argument_or_index() {
    let source = "NAME = \"orders\"\nx = f(NAME)\ny = z[NAME]\n";
    assert_eq!(folds(source).iter().filter(|(t, _)| t == "NAME").count(), 2);
}

#[test]
fn test_should_stay_complete_on_a_type_alias_statement() {
    // a `type X = ...` alias is type-level, so it must not make the analysis give up
    let source = "type Alias = list[str]\nCOLS = [\"a\"]\nprint(COLS)\n";
    let module = parse_module(source).unwrap().into_syntax();
    assert!(Analysis::build(&module, &ImportedModules::new()).complete);
    assert_eq!(used_as(source, "COLS"), Some(strs(&["a"])));
}

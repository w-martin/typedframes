use super::*;

#[test]
fn test_should_not_hand_a_one_shot_iterator_to_a_user_function() {
    let source = "A = [\"a\", \"b\"]\ndef f(it):\n    return list(it) + list(it)\n";
    for call in [
        "f(x for x in A)",
        "f(zip(A, A))",
        "len(zip(A, A))",
        "len(reversed(A))",
        "bool(zip(A, A))",
        "list(reversed(zip(A, A)))",
    ] {
        let source = format!("{source}print({call})\n");

        assert_eq!(folded(&source, call), None, "{call}");
    }
    let ok = format!("{source}print(f([\"x\"]))\n");
    assert_eq!(folded(&ok, "f([\"x\"])"), Some(strs(&["x", "x"])));
}

#[test]
fn test_should_evaluate_a_call_to_a_function_that_returns_a_constant() {
    let source = "def cols():\n    return [\"a\", \"b\"]\n\n\nprint(usecols=cols())\n";
    assert_eq!(folded(source, "cols()"), Some(strs(&["a", "b"])));
}

#[test]
fn test_should_pass_arguments_by_position_by_keyword_and_by_default() {
    let def = "def cols(prefix=\"m\", n=2):\n    return [f\"{prefix}{i}\" for i in range(n)]\n\n\n";
    for (call, expected) in [
        ("cols()", strs(&["m0", "m1"])),
        ("cols(\"z\")", strs(&["z0", "z1"])),
        ("cols(n=1)", strs(&["m0"])),
        ("cols(prefix=\"q\", n=3)", strs(&["q0", "q1", "q2"])),
        ("cols(\"q\", 1)", strs(&["q0"])),
    ] {
        let source = format!("{def}print(usecols={call})\n");
        assert_eq!(folded(&source, call), Some(expected), "{call}");
    }
}

#[test]
fn test_should_run_local_assignments_a_docstring_and_annotations() {
    let source = "def cols() -> list[str]:\n    \"\"\"Doc.\"\"\"\n    base: list[str] = [\"a\"]\n    extra = [\"b\"]\n    base = base + extra\n    return base\n\n\nprint(usecols=cols())\n";
    assert_eq!(folded(source, "cols()"), Some(strs(&["a", "b"])));
}

#[test]
fn test_should_read_module_constants_and_call_other_functions() {
    let source = "BASE = [\"a\"]\n\n\ndef base():\n    return BASE + [\"b\"]\n\n\ndef cols():\n    return base() + [\"c\"]\n\n\nprint(usecols=cols())\n";
    assert_eq!(folded(source, "cols()"), Some(strs(&["a", "b", "c"])));
}

#[test]
fn test_should_give_each_call_a_fresh_default() {
    let source = "def f(x=[]):\n    return x + [\"a\"]\n\n\nprint(a=f(), b=f())\n";
    let found: Vec<Value> = folds(source)
        .into_iter()
        .filter(|(t, _)| t == "f()")
        .map(|(_, v)| v)
        .collect();
    assert_eq!(found, vec![strs(&["a"]), strs(&["a"])]);
}

#[test]
fn test_should_let_a_local_shadow_a_module_constant() {
    let source = "COLS = [\"module\"]\n\n\ndef f():\n    COLS = [\"local\"]\n    return COLS\n\n\nprint(a=f())\n";
    assert_eq!(folded(source, "f()"), Some(strs(&["local"])));
}

#[test]
fn test_should_refuse_a_function_it_cannot_evaluate() {
    for def in [
        // control flow and statements that do anything but assign and return
        "def f():\n    for i in range(2):\n        pass\n    return [\"a\"]",
        "def f():\n    if True:\n        return [\"a\"]\n    return [\"b\"]",
        "def f():\n    while False:\n        pass\n    return [\"a\"]",
        "def f():\n    print(\"x\")\n    return [\"a\"]",
        "def f():\n    x = [\"a\"]\n    x.append(\"b\")\n    return x",
        "def f():\n    try:\n        return [\"a\"]\n    except Exception:\n        return []",
        "def f():\n    with open(\"p\") as h:\n        return [\"a\"]",
        "def f():\n    global G\n    return [\"a\"]",
        "def f():\n    a, b = [1, 2]\n    return [a]",
        "def f():\n    x = [\"a\"]",
        "def f():\n    return [\"a\"]\n    return [\"b\"]",
        "def f():\n    pass",
        // side effects and runtime state
        "def f():\n    return open(\"p\").read().split()",
        "def f():\n    return list(os.environ)",
        "def f():\n    return [g()]",
        // shapes it does not model
        "@cache\ndef f():\n    return [\"a\"]",
        "async def f():\n    return [\"a\"]",
        "def f():\n    yield \"a\"",
        "def f(*args):\n    return [\"a\"]",
        "def f(**kwargs):\n    return [\"a\"]",
        "def f():\n    return f()",
    ] {
        let source = format!("{def}\n\n\nprint(usecols=f())\n");
        assert_eq!(folded(&source, "f()"), None, "{def}");
    }
}

#[test]
fn test_should_refuse_a_call_with_the_wrong_arguments() {
    let def = "def f(a, b=1, *, c=2):\n    return [a, b, c]\n\n\n";
    for call in [
        "f()",
        "f(1, 2, 3)",
        "f(1, d=4)",
        "f(1, a=2)",
        "f(*[1])",
        "f(**{\"a\": 1})",
        "f(unknown)",
        "f(g())",
    ] {
        let source = format!("{def}print(x={call})\n");
        assert_eq!(folded(&source, call), None, "{call}");
    }
    let ok = "def f(a, b=1, *, c=2):\n    return [a, b, c]\n\n\nprint(x=f(0, c=9))\n";
    assert_eq!(
        folded(ok, "f(0, c=9)"),
        Some(Value::List(vec![
            Value::Int(0),
            Value::Int(1),
            Value::Int(9)
        ]))
    );
}

#[test]
fn test_should_refuse_a_function_that_is_defined_twice_or_not_at_all() {
    let twice =
        "def f():\n    return [\"a\"]\n\n\ndef f():\n    return [\"b\"]\n\n\nprint(x=f())\n";
    assert_eq!(folded(twice, "f()"), None);
    assert_eq!(folded("print(x=f())\n", "f()"), None);
    let reassigned = "def f():\n    return [\"a\"]\n\n\nf = other\nprint(x=f())\n";
    assert_eq!(folded(reassigned, "f()"), None);
}

#[test]
fn test_should_refuse_mutual_recursion_and_stop_at_a_call_limit() {
    let mutual = "def a():\n    return b()\n\n\ndef b():\n    return a()\n\n\nprint(x=a())\n";
    assert_eq!(folded(mutual, "a()"), None);
    let chain: String = (0..40)
        .map(|i| format!("def f{i}():\n    return f{}()\n\n\n", i + 1))
        .collect::<String>()
        + "def f40():\n    return [\"end\"]\n\n\nprint(x=f0())\n";
    assert_eq!(folded(&chain, "f0()"), None);
}

#[test]
fn test_should_not_trust_a_function_when_the_module_namespace_can_change() {
    let source = "def f():\n    return [\"a\"]\n\n\nglobals()\nprint(x=f())\n";
    assert_eq!(folded(source, "f()"), None);
}

#[test]
fn test_should_not_let_a_list_escape_through_a_function_it_is_passed_to() {
    // `ident` returns its argument, so ALIAS would be COLS itself
    let source = "COLS = [\"a\"]\n\n\ndef ident(x):\n    return x\n\n\nALIAS = ident(COLS)\nprint(usecols=COLS)\n";
    assert_eq!(used_as(source, "COLS"), None);
    // an immutable argument is fine
    let source = "NAME = \"a\"\n\n\ndef ident(x):\n    return x\n\n\nprint(a=ident(NAME))\n";
    assert_eq!(folded(source, "ident(NAME)"), Some(s("a")));
}

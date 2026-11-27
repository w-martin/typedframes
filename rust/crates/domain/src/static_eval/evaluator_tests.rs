use super::*;
use ruff_python_parser::parse_expression;

// Resolves a handful of fixed names; anything else is a builtin.
struct Env(HashMap<&'static str, Value>);

impl NameResolver for Env {
    fn resolve(&self, name: &ast::ExprName) -> Resolved {
        match self.0.get(name.id.as_str()) {
            Some(v) => Resolved::Value(v.clone()),
            None if name.id.as_str().starts_with("unknown") => Resolved::Unknown,
            None => Resolved::Builtin,
        }
    }
}

fn s(v: &str) -> Value {
    Value::Str(v.to_string())
}

fn strs(items: &[&str]) -> Value {
    Value::List(items.iter().map(|i| s(i)).collect())
}

fn eval(source: &str) -> Option<Value> {
    let env = Env(HashMap::from([
        ("BASE", strs(&["id", "amount"])),
        ("NAME", s("orders")),
        ("N", Value::Int(3)),
        (
            "ROWS",
            Value::Dict(vec![(s("x"), Value::List(vec![Value::Int(1)]))]),
        ),
    ]));
    let parsed = parse_expression(source).unwrap();
    Evaluator::new(&env).eval(parsed.expr())
}

#[test]
fn test_should_evaluate_literals_and_displays() {
    assert_eq!(eval("\"a\""), Some(s("a")));
    assert_eq!(
        eval("[1, 2]"),
        Some(Value::List(vec![Value::Int(1), Value::Int(2)]))
    );
    assert_eq!(
        eval("(\"a\", \"b\")"),
        Some(Value::Tuple(vec![s("a"), s("b")]))
    );
    assert_eq!(
        eval("{\"a\": 1}"),
        Some(Value::Dict(vec![(s("a"), Value::Int(1))]))
    );
    assert_eq!(
        eval("{\"a\", \"a\", \"b\"}"),
        Some(Value::Set(vec![s("a"), s("b")]))
    );
    assert_eq!(eval("None"), Some(Value::None));
    assert_eq!(eval("-5"), Some(Value::Int(-5)));
}

#[test]
fn test_should_evaluate_names_through_the_resolver() {
    assert_eq!(eval("BASE"), Some(strs(&["id", "amount"])));
    assert_eq!(eval("unknown_thing"), None);
}

#[test]
fn test_should_concatenate_and_repeat() {
    assert_eq!(eval("BASE + [\"x\"]"), Some(strs(&["id", "amount", "x"])));
    assert_eq!(eval("[\"a\"] * 2"), Some(strs(&["a", "a"])));
    assert_eq!(eval("2 * \"ab\""), Some(s("abab")));
    assert_eq!(
        eval("(\"a\",) + (\"b\",)"),
        Some(Value::Tuple(vec![s("a"), s("b")]))
    );
    assert_eq!(eval("\"a\" + 1"), None);
}

#[test]
fn test_should_spread_into_displays() {
    assert_eq!(eval("[*BASE, \"x\"]"), Some(strs(&["id", "amount", "x"])));
    assert_eq!(
        eval("{**{\"a\": 1}, \"b\": 2}"),
        Some(Value::Dict(vec![
            (s("a"), Value::Int(1)),
            (s("b"), Value::Int(2))
        ]))
    );
    assert_eq!(eval("{**unknown_thing}"), None);
}

#[test]
fn test_should_format_fstrings_of_scalars_only() {
    assert_eq!(eval("f\"{NAME}_{N}\""), Some(s("orders_3")));
    assert_eq!(eval("f\"a{True}{None}\""), Some(s("aTrueNone")));
    assert_eq!(eval("f\"{BASE}\""), None);
    assert_eq!(eval("f\"{N:03d}\""), None);
    assert_eq!(eval("f\"{N!r}\""), None);
    assert_eq!(eval("f\"{N=}\""), None);
}

#[test]
fn test_should_evaluate_comprehensions() {
    assert_eq!(
        eval("[f\"m{i}\" for i in range(3)]"),
        Some(strs(&["m0", "m1", "m2"]))
    );
    assert_eq!(
        eval("[c.upper() for c in BASE if c != \"id\"]"),
        Some(strs(&["AMOUNT"]))
    );
    assert_eq!(
        eval("{c: len(c) for c in BASE}"),
        Some(Value::Dict(vec![
            (s("id"), Value::Int(2)),
            (s("amount"), Value::Int(6))
        ]))
    );
    assert_eq!(
        eval("[(a, b) for a in [1, 2] for b in \"xy\"]").map(|v| match v {
            Value::List(items) => items.len(),
            _ => 0,
        }),
        Some(4)
    );
    assert_eq!(
        eval("[a + b for a, b in zip([\"x\"], [\"y\"])]"),
        Some(strs(&["xy"]))
    );
}

#[test]
fn test_should_unpack_into_comprehensions() {
    assert_eq!(
        eval("[*g for g in [BASE, [\"x\"]]]"),
        Some(strs(&["id", "amount", "x"]))
    );
    assert_eq!(
        eval("{*g for g in [[\"a\", \"b\"], [\"b\"]]}"),
        Some(Value::Set(vec![s("a"), s("b")]))
    );
    assert_eq!(
        eval("list(*g for g in [BASE, [\"x\"]])"),
        Some(strs(&["id", "amount", "x"]))
    );
    assert_eq!(
        eval("{**m for m in [{\"a\": 1, \"b\": 2}, {\"a\": 3}]}"),
        Some(Value::Dict(vec![
            (s("a"), Value::Int(3)),
            (s("b"), Value::Int(2))
        ]))
    );
    assert_eq!(eval("[*unknown_thing for _ in [1]]"), None);
    assert_eq!(eval("[*N for _ in [1]]"), None);
    assert_eq!(eval("{**g for g in [BASE]}"), None);
}

#[test]
fn test_should_evaluate_frozendict_like_dict() {
    assert_eq!(
        eval("frozendict(a=1)"),
        Some(Value::Dict(vec![(s("a"), Value::Int(1))]))
    );
    assert_eq!(
        eval("frozendict({\"a\": 1}, b=2)"),
        Some(Value::Dict(vec![
            (s("a"), Value::Int(1)),
            (s("b"), Value::Int(2))
        ]))
    );
}

#[test]
fn test_should_scope_a_comprehension_variable_to_its_comprehension() {
    // the outer `N` is still 3 afterwards, and the comprehension's own `N` doesn't leak
    assert_eq!(
        eval("[N for N in [1, 2]] + [N]"),
        Some(Value::List(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3)
        ]))
    );
    // the first iterable is evaluated in the enclosing scope
    assert_eq!(
        eval("[N for N in range(N)]")
            .map(|v| v == Value::List(vec![Value::Int(0), Value::Int(1), Value::Int(2)])),
        Some(true)
    );
}

#[test]
fn test_should_evaluate_a_generator_only_as_a_builtin_argument() {
    assert_eq!(
        eval("sorted(c for c in BASE)"),
        Some(strs(&["amount", "id"]))
    );
    assert_eq!(eval("\",\".join(c for c in BASE)"), Some(s("id,amount")));
    assert_eq!(eval("(c for c in BASE)"), None);
}

#[test]
fn test_should_evaluate_builtins() {
    assert_eq!(eval("len(BASE)"), Some(Value::Int(2)));
    assert_eq!(eval("sorted(BASE)"), Some(strs(&["amount", "id"])));
    assert_eq!(
        eval("sorted(BASE, reverse=True)"),
        Some(strs(&["id", "amount"]))
    );
    assert_eq!(eval("list(reversed(BASE))"), Some(strs(&["amount", "id"])));
    assert_eq!(eval("min(BASE)"), Some(s("amount")));
    assert_eq!(eval("max(1, 5, 3)"), Some(Value::Int(5)));
    assert_eq!(eval("sum([1, 2, 3])"), Some(Value::Int(6)));
    assert_eq!(
        eval("list(range(2, 8, 3))"),
        Some(Value::List(vec![Value::Int(2), Value::Int(5)]))
    );
    assert_eq!(eval("str(N)"), Some(s("3")));
    assert_eq!(eval("int(\" 12 \")"), Some(Value::Int(12)));
    assert_eq!(eval("any([0, 1])"), Some(Value::Bool(true)));
    assert_eq!(eval("all([])"), Some(Value::Bool(true)));
    assert_eq!(
        eval("dict(a=1)"),
        Some(Value::Dict(vec![(s("a"), Value::Int(1))]))
    );
    assert_eq!(
        eval("dict([(\"a\", 1)])"),
        Some(Value::Dict(vec![(s("a"), Value::Int(1))]))
    );
    assert_eq!(
        eval("list(enumerate(BASE, start=1))"),
        Some(Value::List(vec![
            Value::Tuple(vec![Value::Int(1), s("id")]),
            Value::Tuple(vec![Value::Int(2), s("amount")]),
        ]))
    );
}

#[test]
fn test_should_refuse_unknown_calls_and_shadowed_builtins() {
    assert_eq!(eval("open(\"f\")"), None);
    assert_eq!(eval("unknown_fn(1)"), None);
    assert_eq!(eval("os.environ"), None);
    // a name the resolver knows about is not the builtin
    assert_eq!(eval("N(1)"), None);
}

#[test]
fn test_should_evaluate_string_methods() {
    assert_eq!(eval("\"a,b\".split(\",\")"), Some(strs(&["a", "b"])));
    assert_eq!(eval("\"a b  c\".split()"), Some(strs(&["a", "b", "c"])));
    assert_eq!(eval("\"a,b,c\".split(\",\")[1]"), Some(s("b")));
    assert_eq!(eval("\",\".join(BASE)"), Some(s("id,amount")));
    assert_eq!(eval("\"Ab\".lower()"), Some(s("ab")));
    assert_eq!(eval("\"hello world\".title()"), Some(s("Hello World")));
    assert_eq!(eval("\"x_y\".replace(\"_\", \" \")"), Some(s("x y")));
    assert_eq!(eval("\"  a \".strip()"), Some(s("a")));
    assert_eq!(eval("\"--a--\".strip(\"-\")"), Some(s("a")));
    assert_eq!(eval("\"abc\".startswith(\"ab\")"), Some(Value::Bool(true)));
    assert_eq!(
        eval("\"abc\".endswith((\"x\", \"bc\"))"),
        Some(Value::Bool(true))
    );
    assert_eq!(eval("\"pre_a\".removeprefix(\"pre_\")"), Some(s("a")));
    assert_eq!(eval("\"a\".bogus()"), None);
    assert_eq!(eval("\"a\".replace(\"\", \"x\")"), None);
}

#[test]
fn test_should_evaluate_str_format() {
    assert_eq!(eval("\"{}_{}\".format(\"a\", 1)"), Some(s("a_1")));
    assert_eq!(eval("\"{1}{0}\".format(\"a\", \"b\")"), Some(s("ba")));
    assert_eq!(eval("\"{n}x\".format(n=NAME)"), Some(s("ordersx")));
    assert_eq!(eval("\"{{}}\".format()"), Some(s("{}")));
    assert_eq!(eval("\"{}\".format()"), None);
    assert_eq!(eval("\"{:>5}\".format(1)"), None);
    assert_eq!(eval("\"{a.b}\".format(a=1)"), None);
}

#[test]
fn test_should_evaluate_dict_methods_and_subscripts() {
    assert_eq!(
        eval("list({\"a\": 1, \"b\": 2}.keys())"),
        Some(strs(&["a", "b"]))
    );
    assert_eq!(eval("{\"a\": 1}.get(\"z\", 7)"), Some(Value::Int(7)));
    assert_eq!(eval("{\"a\": 1}[\"a\"]"), Some(Value::Int(1)));
    assert_eq!(eval("{\"a\": 1}[\"z\"]"), None);
    assert_eq!(eval("BASE[0]"), Some(s("id")));
    assert_eq!(eval("BASE[-1]"), Some(s("amount")));
    assert_eq!(eval("BASE[5]"), None);
    assert_eq!(
        eval("[1, 2, 3, 4][1:3]"),
        Some(Value::List(vec![Value::Int(2), Value::Int(3)]))
    );
    assert_eq!(
        eval("[1, 2, 3][::-1]"),
        Some(Value::List(vec![
            Value::Int(3),
            Value::Int(2),
            Value::Int(1)
        ]))
    );
    assert_eq!(eval("\"abcd\"[1:3]"), Some(s("bc")));
}

#[test]
fn test_should_evaluate_comparisons_and_conditions() {
    assert_eq!(eval("1 < 2 < 3"), Some(Value::Bool(true)));
    assert_eq!(eval("1 < 2 > 5"), Some(Value::Bool(false)));
    assert_eq!(eval("\"id\" in BASE"), Some(Value::Bool(true)));
    assert_eq!(eval("\"z\" not in BASE"), Some(Value::Bool(true)));
    assert_eq!(eval("None is None"), Some(Value::Bool(true)));
    assert_eq!(eval("BASE is None"), Some(Value::Bool(false)));
    assert_eq!(eval("BASE is BASE"), None);
    assert_eq!(eval("[\"a\"] if N > 2 else [\"b\"]"), Some(strs(&["a"])));
    assert_eq!(eval("[] or [\"d\"]"), Some(strs(&["d"])));
    assert_eq!(eval("N and \"x\""), Some(s("x")));
    assert_eq!(eval("not BASE"), Some(Value::Bool(false)));
    assert_eq!(eval("True == 1"), Some(Value::Bool(true)));
}

#[test]
fn test_should_not_evaluate_the_untaken_branch() {
    assert_eq!(
        eval("[\"a\"] if True else unknown_thing"),
        Some(strs(&["a"]))
    );
    assert_eq!(eval("True or unknown_thing"), Some(Value::Bool(true)));
}

#[test]
fn test_should_do_set_operations_but_never_order_a_set() {
    assert_eq!(
        eval("{\"a\", \"b\"} | {\"c\"}"),
        Some(Value::Set(vec![s("a"), s("b"), s("c")]))
    );
    assert_eq!(
        eval("{\"a\", \"b\"} & {\"b\"}"),
        Some(Value::Set(vec![s("b")]))
    );
    assert_eq!(
        eval("{\"a\", \"b\"} - {\"b\"}"),
        Some(Value::Set(vec![s("a")]))
    );
    assert_eq!(eval("sorted({\"b\", \"a\"})"), Some(strs(&["a", "b"])));
    assert_eq!(eval("len({\"a\", \"b\"})"), Some(Value::Int(2)));
    assert_eq!(eval("set([\"a\", \"a\"])"), Some(Value::Set(vec![s("a")])));
    // order-dependent uses are refused
    assert_eq!(eval("list({\"a\", \"b\"})"), None);
    assert_eq!(eval("[c for c in {\"a\"}]"), None);
    assert_eq!(eval("[*{\"a\"}]"), None);
    assert_eq!(eval("{\"a\", \"b\"}[0]"), None);
    // but an unordered consumer is fine
    assert_eq!(
        eval("{c.upper() for c in {\"a\"}}"),
        Some(Value::Set(vec![s("A")]))
    );
}

#[test]
fn test_should_refuse_unhashable_set_elements_and_dict_keys() {
    assert_eq!(eval("{[1]}"), None);
    assert_eq!(eval("{[1]: 2}"), None);
}

#[test]
fn test_should_refuse_what_it_cannot_decide() {
    assert_eq!(eval("1.5"), None);
    assert_eq!(eval("b\"x\""), None);
    assert_eq!(eval("lambda: 1"), None);
    assert_eq!(eval("(x := 1)"), None);
    assert_eq!(eval("5 / 2"), None);
    assert_eq!(eval("\"%s\" % 1"), None);
    assert_eq!(eval("sorted([1, \"a\"])"), None);
    assert_eq!(eval("1 + True"), Some(Value::Int(2)));
}

#[test]
fn test_should_refuse_values_that_cannot_all_be_ordered() {
    assert_eq!(eval("sorted([(0, \"a\"), (1, 2), (0, 5)])"), None);
    assert_eq!(
        eval("sorted([(1, 2), (0, 5)])"),
        Some(Value::List(vec![
            Value::Tuple(vec![Value::Int(0), Value::Int(5)]),
            Value::Tuple(vec![Value::Int(1), Value::Int(2)]),
        ]))
    );
}

#[test]
fn test_should_only_evaluate_one_shot_iterators_where_they_are_consumed() {
    assert_eq!(eval("zip([\"a\"], [\"b\"])"), None);
    assert_eq!(eval("reversed([\"a\"])"), None);
    assert_eq!(eval("enumerate([\"a\"])"), None);
    assert_eq!(
        eval("[x for x in reversed([\"a\", \"b\"])]"),
        Some(strs(&["b", "a"]))
    );
}

#[test]
fn test_should_not_overflow_on_an_extreme_slice_step() {
    assert_eq!(
        eval("[\"a\", \"b\"][0::9223372036854775807]"),
        Some(strs(&["a"]))
    );
    assert_eq!(
        eval("[\"a\", \"b\"][::-9223372036854775807]"),
        Some(strs(&["b"]))
    );
}

#[test]
fn test_should_title_case_around_uncased_letters_like_python() {
    assert_eq!(eval("\"\u{4e2d}a b\".title()"), Some(s("\u{4e2d}A B")));
    assert_eq!(eval("\"stra\u{df}e\".title()"), None);
}

#[test]
fn test_should_stop_at_the_size_and_step_limits() {
    assert_eq!(eval("range(1000000000)"), None);
    assert_eq!(eval("range(10 ** 9)"), None); // `**` itself is unsupported
    assert_eq!(eval("range(20000)"), None);
    assert_eq!(eval("\"a\" * 1000000"), None);
    assert_eq!(eval("[0] * 20000"), None);
    assert_eq!(
        eval("[i for i in range(10000) for j in range(10000)]"),
        None
    );
    assert_eq!(eval("1 + 9223372036854775807"), None);
}

#[test]
fn test_should_report_the_string_list_of_a_column_list_value() {
    assert_eq!(
        strs(&["a", "b"]).as_string_list(),
        Some(vec!["a".to_string(), "b".to_string()])
    );
    assert_eq!(Value::List(vec![Value::Int(1)]).as_string_list(), None);
    assert_eq!(s("a").as_string_list(), None);
}

#[test]
fn test_should_tell_flat_values_from_values_with_mutable_elements() {
    assert!(strs(&["a"]).has_immutable_elements());
    assert!(Value::Tuple(vec![Value::Tuple(vec![Value::Int(1)])]).has_immutable_elements());
    assert!(Value::Dict(vec![(s("a"), Value::Int(1))]).has_immutable_elements());
    assert!(!Value::List(vec![strs(&["a"])]).has_immutable_elements());
    assert!(!Value::Dict(vec![(s("a"), strs(&["a"]))]).has_immutable_elements());
}

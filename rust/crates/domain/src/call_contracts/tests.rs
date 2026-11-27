use super::*;
use ruff_python_parser::parse_module;

fn contract_of(source: &str) -> FuncContract {
    let module = parse_module(source).unwrap().into_syntax();
    let Stmt::FunctionDef(def) = &module.body[0] else {
        panic!("expected a function");
    };
    derive(def, false, 1, &|_| None)
}

#[test]
fn test_should_require_reads_of_every_parameter_before_any_write() {
    let c = contract_of(
            "def f(a, b, *, c):\n    x = b[\"y\"] + c[[\"p\", \"q\"]]\n    b[\"z\"] = 1\n    return b[\"z\"] + a[\"k\"]\n",
        );
    assert_eq!(c.requires[1], vec!["y".to_string()]);
    assert_eq!(c.requires[2], vec!["p".to_string(), "q".to_string()]);
    assert_eq!(c.requires[0], vec!["k".to_string()]);
}

#[test]
fn test_should_not_require_reads_after_a_branch_or_inside_one() {
    let c = contract_of(
        "def f(a, b):\n    if \"y\" in b.columns:\n        return b[\"y\"]\n    return b[\"z\"]\n",
    );
    assert!(c.requires[1].is_empty(), "{:?}", c.requires);
}

#[test]
fn test_should_not_require_a_guarded_read_of_the_first_parameter() {
    let guarded = contract_of(
        "def maybe(df):\n    if \"a\" in df.columns:\n        return df[\"a\"]\n    return None\n",
    );
    let ternary = contract_of("def f(df):\n    return df[\"a\"] if \"a\" in df else None\n");
    let straight = contract_of("def f(df):\n    x = df[\"k\"] + 1\n    return df[\"v\"] * x\n");
    assert!(guarded.requires[0].is_empty(), "{:?}", guarded.requires);
    assert!(ternary.requires[0].is_empty(), "{:?}", ternary.requires);
    assert_eq!(straight.requires[0], vec!["k".to_string(), "v".to_string()]);
}

#[test]
fn test_should_require_polars_columns_an_expression_names_on_the_parameter() {
    let c = contract_of(
            "def g(x, frame):\n    out = frame.group_by(\"d\").agg(pl.col(\"p\").sum())\n    return out\n",
        );
    assert_eq!(c.requires[1], vec!["p".to_string()]);
}

#[test]
fn test_should_stop_at_a_call_that_may_change_the_frame() {
    let c = contract_of("def f(a, b):\n    add(b)\n    return b[\"new\"]\n");
    assert!(c.requires[1].is_empty());
    assert_eq!(c.forwards.len(), 1);
    assert_eq!(c.forwards[0].param, 1);
}

#[test]
fn test_should_bind_keywords_and_reject_unknown_ones() {
    let sig = Signature {
        positional: vec!["a".into(), "b".into()],
        posonly: 0,
        kwonly: vec!["c".into()],
        varargs: false,
        kwargs: false,
    };
    let shape = CallShape {
        positional: 1,
        keywords: vec!["c".into(), "b".into()],
    };
    assert_eq!(sig.bind(&shape), Some(vec![Some(0), Some(2), Some(1)]));
    let bad = CallShape {
        positional: 1,
        keywords: vec!["a".into()],
    };
    assert_eq!(sig.bind(&bad), None);
    let extra = CallShape {
        positional: 3,
        keywords: vec![],
    };
    assert_eq!(sig.bind(&extra), None);
}

use super::*;
use ruff_python_ast::Stmt;
use ruff_python_parser::parse_module;

fn classify(rhs: &str) -> Option<Derivation> {
    let source = format!("x = {rhs}\n");
    let module = parse_module(&source).unwrap().into_syntax();
    let Stmt::Assign(assign) = &module.body[0] else {
        panic!("expected an assignment");
    };
    classify_rhs(&assign.value, &|name| name == "df" || name == "other")
}

fn kind(rhs: &str) -> Option<(String, Kind)> {
    classify(rhs).map(|derived| (derived.root, derived.kind))
}

fn df(kind: Kind) -> Option<(String, Kind)> {
    Some(("df".to_string(), kind))
}

fn passthrough(source: &str) -> bool {
    let module = parse_module(source).unwrap().into_syntax();
    let Stmt::FunctionDef(func_def) = &module.body[0] else {
        panic!("expected a function");
    };
    summarize_helper(func_def).is_some_and(|s| s.returned.is_some())
}

fn edits(source: &str) -> Option<Vec<ColumnEdit>> {
    let module = parse_module(source).unwrap().into_syntax();
    let Stmt::FunctionDef(func_def) = &module.body[0] else {
        panic!("expected a function");
    };
    summarize_helper(func_def).and_then(|s| s.returned)
}

fn strings(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| n.to_string()).collect()
}

fn in_place(source: &str) -> Option<Vec<ColumnEdit>> {
    let module = parse_module(source).unwrap().into_syntax();
    let Stmt::FunctionDef(func_def) = &module.body[0] else {
        panic!("expected a function");
    };
    summarize_helper(func_def).map(|s| s.in_place)
}

fn delegate(name: &str, via_self: bool, conditional: bool) -> ColumnEdit {
    ColumnEdit::Delegate {
        callee: FnRef {
            via_self,
            module: None,
            name: name.to_string(),
        },
        conditional,
    }
}

fn summary(in_place: Vec<ColumnEdit>) -> HelperSummary {
    HelperSummary {
        returned: None,
        in_place,
    }
}

fn merged(args: &str) -> Merged {
    let source = format!("x = left.merge(right{args})\n");
    let module = parse_module(&source).unwrap().into_syntax();
    let Stmt::Assign(assign) = &module.body[0] else {
        panic!("expected an assignment");
    };
    let Expr::Call(call) = &*assign.value else {
        panic!("expected a call");
    };
    let owned = |cols: &[&str]| cols.iter().map(|c| c.to_string()).collect::<Vec<_>>();
    merged_columns(
        &owned(&["user_id", "segment", "ctr"]),
        &owned(&["segment", "ctr", "size"]),
        call,
    )
}

fn merged_set(args: &str) -> Vec<String> {
    match merged(args) {
        Merged::Columns(mut columns) => {
            columns.sort();
            columns
        }
        Merged::Unknown => panic!("expected columns"),
    }
}

#[test]
fn test_should_treat_schema_preserving_methods_as_preserving() {
    for rhs in [
        "df.copy()",
        "df.drop_duplicates()",
        "df.astype({'a': 'int'})",
        "df.head().reset_index()",
        "df.copy().sort_values('a').head(3)",
    ] {
        assert_eq!(kind(rhs), df(Kind::Preserves), "{rhs}");
    }
}

#[test]
fn test_should_treat_unknown_methods_and_functions_as_derived() {
    for rhs in [
        "df.some_unknown()",
        "df.groupby('a').agg({'b': 'sum'})",
        "df.head().some_unknown()",
        "clean(df)",
        "module.clean(df, 3)",
        "pd.concat([df, new_rows])",
        "df.T",
        "df if cond else other_thing()",
    ] {
        assert_eq!(kind(rhs), df(Kind::Derived), "{rhs}");
    }
}

#[test]
fn test_should_treat_series_and_scalar_results_as_not_a_frame() {
    for rhs in [
        "df.sum()",
        "df['a']",
        "df['a'].map(f)",
        "df.columns",
        "df.to_dict()",
        "df.loc[:, 'a']",
        "df.iloc[0]",
        "df.at[0, 'a']",
    ] {
        assert_eq!(kind(rhs), df(Kind::NotAFrame), "{rhs}");
    }
}

#[test]
fn test_should_keep_columns_for_row_selections() {
    for rhs in [
        "df[df['a'] > 1]",
        "df[mask]",
        "df.loc[df['a'] > 1]",
        "df.loc[mask, :]",
        "df.iloc[:10]",
        "df.head()[mask]",
    ] {
        assert_eq!(kind(rhs), df(Kind::Preserves), "{rhs}");
    }
}

#[test]
fn test_should_treat_column_selections_as_derived() {
    for rhs in ["df.loc[:, ['a', 'b']]", "df.head()[['a', 'b']]"] {
        assert_eq!(kind(rhs), df(Kind::Derived), "{rhs}");
    }
}

#[test]
fn test_should_not_derive_from_untracked_names() {
    for rhs in ["load_other()", "untracked.copy()", "f(untracked)", "1 + 2"] {
        assert_eq!(kind(rhs), None, "{rhs}");
    }
}

#[test]
fn test_should_ignore_a_non_frame_first_argument() {
    assert_eq!(kind("f(df['a'])"), None);
}

#[test]
fn test_should_describe_the_operation_for_diagnostics() {
    let describe = |rhs: &str| {
        let source = format!("x = {rhs}\n");
        let module = parse_module(&source).unwrap().into_syntax();
        let Stmt::Assign(assign) = &module.body[0] else {
            panic!("expected an assignment");
        };
        describe_rhs(&assign.value)
    };
    assert_eq!(describe("clean(df)"), "`clean()`");
    assert_eq!(describe("df.groupby('a').agg(f)"), "`.agg()`");
    assert_eq!(describe("await fetch(df)"), "`fetch()`");
    assert_eq!(describe("df['a']"), "this subscript");
    assert_eq!(describe("df.T"), "`.T`");
    assert_eq!(describe("(lambda: 1)()"), "this call");
    assert_eq!(describe("a + b"), "this expression");
}

#[test]
fn test_should_infer_a_helper_that_returns_its_frame_unchanged() {
    assert!(passthrough(
        "def clean(df):\n    return df.dropna().reset_index()\n"
    ));
    assert!(passthrough(
        "def clean(df):\n    df = df.copy()\n    df = df.head(3)\n    return df\n"
    ));
    assert!(passthrough(
        "def clean(self, df):\n    if x:\n        return df\n    return df.copy()\n"
    ));
}

#[test]
fn test_should_not_infer_a_helper_that_changes_or_drops_the_frame() {
    for body in [
        "return df.groupby('a').agg(f)",
        "return other",
        "return",
        "pass",
        "df = df.merge(x)\n    return df",
        "df, y = split(df)\n    return df",
        "for df in dfs:\n        pass\n    return df",
        "return df['a']",
        "df: object = other\n    return df",
        "with opener() as df:\n        pass\n    return df",
        "del df\n    return df",
        "(df := other)\n    return df",
    ] {
        let source = format!("def clean(df):\n    {body}\n");
        assert!(!passthrough(&source), "{body}");
    }
    assert!(!passthrough("def clean():\n    return 1\n"));
}

#[test]
fn test_should_only_count_edits_made_before_the_parameter_is_rebound() {
    let before =
        "def f(df):\n    df['a'] = 1\n    df = df.copy()\n    df['b'] = 2\n    return df\n";
    assert_eq!(
        in_place(before),
        Some(vec![ColumnEdit::Add(strings(&["a"]))])
    );
    assert_eq!(
        edits(before),
        Some(vec![
            ColumnEdit::Add(strings(&["a"])),
            ColumnEdit::Add(strings(&["b"]))
        ])
    );
    assert_eq!(
        in_place("def f(df):\n    df = df.copy()\n    df['b'] = 2\n    return df\n"),
        Some(Vec::new())
    );
}

#[test]
fn test_should_treat_edits_after_a_conditional_rebind_as_unknown_in_place() {
    let source = "def f(df, c):\n    if c:\n        df = df.copy()\n    df['b'] = 2\n";
    assert_eq!(in_place(source), Some(vec![ColumnEdit::Unknown]));
}

#[test]
fn test_should_resolve_delegates_through_bare_and_self_callees() {
    let class = Some("C".to_string());
    let mut summaries = HashMap::from([
        (
            (None, "leaf".to_string()),
            summary(vec![ColumnEdit::Add(strings(&["z"]))]),
        ),
        (
            (class.clone(), "leaf".to_string()),
            summary(vec![ColumnEdit::Add(strings(&["m"]))]),
        ),
        (
            (class.clone(), "top".to_string()),
            summary(vec![
                delegate("leaf", true, false),
                delegate("leaf", false, false),
            ]),
        ),
    ]);

    resolve_delegates(&mut summaries, &HashMap::new(), false);

    assert_eq!(
        summaries[&(class, "top".to_string())].in_place,
        vec![
            ColumnEdit::Add(strings(&["m"])),
            ColumnEdit::Add(strings(&["z"]))
        ]
    );
}

#[test]
fn test_should_drop_delegates_to_unknown_callees_and_self_calls_outside_a_class() {
    let mut summaries = HashMap::from([
        (
            (None, "f".to_string()),
            summary(vec![
                delegate("missing", false, false),
                delegate("m", true, false),
                ColumnEdit::Add(strings(&["a"])),
            ]),
        ),
        (
            (None, "only_calls".to_string()),
            summary(vec![delegate("missing", false, false)]),
        ),
    ]);

    resolve_delegates(&mut summaries, &HashMap::new(), false);

    assert_eq!(
        summaries[&(None, "f".to_string())].in_place,
        vec![ColumnEdit::Add(strings(&["a"]))]
    );
    assert!(!summaries.contains_key(&(None, "only_calls".to_string())));
}

#[test]
fn test_should_make_a_conditional_or_cyclic_delegation_unknown() {
    let mut summaries = HashMap::from([
        (
            (None, "leaf".to_string()),
            summary(vec![ColumnEdit::Add(strings(&["z"]))]),
        ),
        (
            (None, "cond".to_string()),
            summary(vec![delegate("leaf", false, true)]),
        ),
        (
            (None, "a".to_string()),
            summary(vec![delegate("b", false, false)]),
        ),
        (
            (None, "b".to_string()),
            summary(vec![delegate("a", false, false)]),
        ),
    ]);

    resolve_delegates(&mut summaries, &HashMap::new(), false);

    assert_eq!(
        summaries[&(None, "cond".to_string())].in_place,
        vec![ColumnEdit::Unknown]
    );
    assert_eq!(
        summaries[&(None, "a".to_string())].in_place,
        vec![ColumnEdit::Unknown]
    );
}

#[test]
fn test_should_summarize_a_helper_that_only_edits_in_place() {
    let source = "def f(df):\n    df['a'] = 1\n";
    let module = parse_module(source).unwrap().into_syntax();
    let Stmt::FunctionDef(func_def) = &module.body[0] else {
        panic!("expected a function");
    };
    assert_eq!(
        summarize_helper(func_def),
        Some(HelperSummary {
            returned: None,
            in_place: vec![ColumnEdit::Add(strings(&["a"]))],
        })
    );
}

#[test]
fn test_should_collect_the_column_edits_a_helper_makes() {
    let source = "def f(df):\n    df['a'] = 1\n    df[['b', 'c']] = 2\n    df.loc[m, 'd'] = 3\n    del df['a']\n    df.pop('b')\n    df.insert(0, 'e', 1)\n    df.drop(columns=['c'], inplace=True)\n    df.rename(columns={'d': 'x'}, inplace=True)\n    df.columns = ['p', 'q']\n    df.dropna(inplace=True)\n    df.drop([0], inplace=True)\n    return df\n";
    assert_eq!(
        edits(source),
        Some(vec![
            ColumnEdit::Add(strings(&["a"])),
            ColumnEdit::Add(strings(&["b", "c"])),
            ColumnEdit::Add(strings(&["d"])),
            ColumnEdit::Remove(strings(&["a"])),
            ColumnEdit::Remove(strings(&["b"])),
            ColumnEdit::Add(strings(&["e"])),
            ColumnEdit::Remove(strings(&["c"])),
            ColumnEdit::Rename(HashMap::from([("d".to_string(), "x".to_string())])),
            ColumnEdit::Replace(strings(&["p", "q"])),
        ])
    );
}

#[test]
fn test_should_treat_unreadable_or_conditional_edits_as_unknown() {
    for body in [
        "df[name] = 1",
        "del df[name]",
        "df.pop(name)",
        "df.insert(0, name, 1)",
        "df.columns = names",
        "df.set_index('a', inplace=True)",
        "df.drop(columns=names, inplace=True)",
        "df.drop(columns=['a'], inplace=flag)",
        "if flag:\n        df['a'] = 1",
        "for x in xs:\n        df[x] = 1",
    ] {
        let source = format!("def f(df, flag=False):\n    {body}\n    return df\n");
        let found = edits(&source).unwrap_or_else(|| panic!("{body}"));
        assert!(found.contains(&ColumnEdit::Unknown), "{body}: {found:?}");
    }
}

#[test]
fn test_should_ignore_inplace_false_and_edits_on_other_names() {
    let source = "def f(df, other):\n    other['a'] = 1\n    df.drop(columns=['a'], inplace=False)\n    return df\n";
    assert_eq!(edits(source), Some(Vec::new()));
}

#[test]
fn test_should_apply_edits_in_order_and_stop_at_unknown() {
    let base = strings(&["a", "b"]);
    let applied = apply_edits(
        &base,
        &[
            ColumnEdit::Add(strings(&["c", "a"])),
            ColumnEdit::Remove(strings(&["b"])),
            ColumnEdit::Rename(HashMap::from([("c".to_string(), "d".to_string())])),
        ],
    );
    assert_eq!(applied, Some(strings(&["a", "d"])));
    assert_eq!(
        apply_edits(&base, &[ColumnEdit::Replace(strings(&["x"]))]),
        Some(strings(&["x"]))
    );
    assert_eq!(apply_edits(&base, &[ColumnEdit::Unknown]), None);
}

#[test]
fn test_should_ignore_returns_and_bindings_in_nested_scopes() {
    let source = "def clean(df):\n    def inner(df):\n        df = f(df)\n        return 1\n    return df.copy()\n";
    assert!(passthrough(source));
}

#[test]
fn test_should_unwrap_await() {
    assert_eq!(kind("await fetch(df)"), df(Kind::Derived));
}

#[test]
fn test_should_suffix_an_overlapping_non_key_column_by_default() {
    // arrange / act / assert
    assert_eq!(
        merged_set(", on=\"segment\""),
        ["ctr_x", "ctr_y", "segment", "size", "user_id"]
    );
}

#[test]
fn test_should_apply_explicit_suffixes() {
    assert_eq!(
        merged_set(", on=\"segment\", suffixes=(\"\", \"_seg\")"),
        ["ctr", "ctr_seg", "segment", "size", "user_id"]
    );
}

#[test]
fn test_should_treat_every_common_column_as_a_key_without_on() {
    assert_eq!(merged_set(""), ["ctr", "segment", "size", "user_id"]);
}

#[test]
fn test_should_merge_a_shared_name_given_as_both_left_on_and_right_on() {
    assert_eq!(
        merged_set(", left_on=\"segment\", right_on=\"segment\""),
        ["ctr_x", "ctr_y", "segment", "size", "user_id"]
    );
}

#[test]
fn test_should_admit_both_spellings_when_the_keys_are_not_literal() {
    let columns = merged_set(", on=KEYS");
    assert!(columns.contains(&"ctr".to_string()) && columns.contains(&"ctr_x".to_string()));
}

#[test]
fn test_should_give_up_on_unreadable_suffixes_only_when_there_is_an_overlap() {
    assert_eq!(merged(", on=\"segment\", suffixes=SFX"), Merged::Unknown);
    assert!(matches!(
        merged(", on=[\"segment\", \"ctr\"], suffixes=SFX"),
        Merged::Columns(_)
    ));
}

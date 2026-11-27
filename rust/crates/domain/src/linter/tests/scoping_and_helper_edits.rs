use super::*;

#[test]
fn test_should_not_leak_a_function_local_frame_into_module_scope() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\ndef g():\n    df = pd.read_csv(\"a.csv\", usecols=[\"a\", \"b\"])\n\n\ndf = build()\nprint(df[\"zzz\"])\n",
        );

    // assert
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_not_leak_a_local_frame_between_methods() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\nclass C:\n    def m1(self):\n        df = pd.read_csv(\"a.csv\", usecols=[\"a\", \"b\"])\n        return df\n\n    def m2(self, p):\n        df = p.load()\n        return df[\"zzz\"]\n",
        );

    // assert
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_let_a_parameter_shadow_a_module_level_frame() {
    // arrange / act
    let (_, errors) = lint_for_leg_tests("def f(df):\n    return df[\"zzz\"]\n");

    // assert
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_still_read_a_module_level_frame_from_inside_a_function() {
    // arrange / act
    let (_, errors) = lint_for_leg_tests("def f():\n    return df[\"zzz\"]\n");

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
}

#[test]
fn test_should_apply_a_global_rebinding_to_module_scope() {
    // arrange / act
    let (_, errors) = lint_for_leg_tests(
        "def f():\n    global df\n    df = df.pipe(g)\n\n\nprint(df[\"zzz\"])\n",
    );

    // assert: the module frame is unresolved after `f` rebinds it.
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
}

#[test]
fn test_should_scope_a_rebinding_inside_a_function_to_that_function() {
    // arrange / act
    let (_, errors) = lint_for_leg_tests(
        "def f():\n    df = other()\n    return df[\"zzz\"]\n\n\nprint(df[\"zzz\"])\n",
    );

    // assert: only the module-level read is checked.
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].line, 8);
}

#[test]
fn test_should_not_count_a_row_mask_write_as_a_helper_column_edit() {
    // arrange / act
    let (linter, errors) = lint_source(
            "import pandas as pd\n\n\ndef fix(df):\n    df[df[\"a\"] > 1] = 0\n    df[1:3] = 0\n    return df\n\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\ndf = fix(df)\nprint(df[\"a\"])\n",
        );

    // assert
    assert_eq!(linter.dataframes_total, 1);
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_apply_a_helpers_in_place_edit_when_called_as_a_statement() {
    // arrange / act
    let (linter, errors) = lint_source(
            "import pandas as pd\n\n\ndef add_features(df):\n    df[\"z\"] = 1\n\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\nadd_features(df)\nprint(df[\"z\"])\nprint(df[\"nope\"])\n",
        );

    // assert: `z` now exists, `nope` does not; the edit is one new leg.
    assert_eq!(linter.dataframes_total, 2);
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'nope'"));
}

#[test]
fn test_should_apply_a_method_helpers_in_place_edit_called_on_self() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\nclass Job:\n    def _add(self, df):\n        df.drop(columns=[\"a\"], inplace=True)\n        df[\"z\"] = 1\n\n    def run(self):\n        df = pd.read_csv(\"a.csv\", usecols=[\"a\", \"b\"])\n        self._add(df)\n        print(df[\"z\"], df[\"b\"])\n        print(df[\"a\"])\n",
        );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'a'"));
}

#[test]
fn test_should_apply_an_in_place_edit_through_await_as_a_statement() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\nasync def add(df):\n    df[\"z\"] = 1\n\n\nasync def main():\n    df = pd.read_csv(\"a.csv\", usecols=[\"a\"])\n    await add(df)\n    print(df[\"z\"])\n",
        );

    // assert
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_not_apply_edits_a_helper_makes_to_its_own_copy() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\ndef add(df):\n    df = df.copy()\n    df[\"z\"] = 1\n    return df\n\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\nout = add(df)\nadd(df)\nprint(out[\"z\"])\nprint(df[\"z\"])\n",
        );

    // assert: the result has `z`; the caller's `df` does not, whether or not the
    // result was kept.
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].line, 14);
}

#[test]
fn test_should_edit_both_the_argument_and_the_result_of_an_in_place_helper_once() {
    // arrange / act
    let (linter, errors) = lint_source(
            "import pandas as pd\n\n\ndef add(df):\n    df[\"z\"] = 1\n    return df\n\n\nx = pd.read_csv(\"a.csv\", usecols=[\"a\"])\ny = add(x)\nprint(x[\"z\"], y[\"z\"])\n",
        );

    // assert: same object, so one new leg, and both names see the column.
    assert_eq!(linter.dataframes_total, 2);
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_apply_the_in_place_edit_when_a_helper_that_returns_nothing_is_assigned() {
    // arrange / act: `add` returns `None`, so `df = add(df)` binds `df` to `None` at
    // the return-value level -- but `add` still mutates the object `df` already
    // named, and `df` is the only name in play (`df = ...` rebinds it to the very
    // thing it already pointed at), so the in-place edit is still `df`'s own.
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\ndef add(df):\n    df[\"z\"] = 1\n\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\ndf = add(df)\nprint(df[\"z\"])\nprint(df[\"zzz\"])\n",
        );

    // assert: `z` was added, `zzz` genuinely is not a column -- a real unknown-column,
    // not merely unverifiable.
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
    assert!(errors[0].message.contains("'zzz'"));
}

#[test]
fn test_should_mark_unresolved_after_a_conditionally_copied_helper_edit() {
    // arrange / act: whether the edit reaches the caller depends on `flag`.
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\ndef add(df, flag):\n    if flag:\n        df = df.copy()\n    df[\"z\"] = 1\n\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\nadd(df, True)\nprint(df[\"z\"])\n",
        );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
}

#[test]
fn test_should_follow_a_helper_that_passes_the_frame_to_another_mutating_helper() {
    // arrange / act
    let (linter, errors) = lint_source(
            "import pandas as pd\n\n\ndef _fix(df):\n    df[\"z\"] = 1\n\n\ndef normalize(df):\n    _fix(df)\n    df[\"y\"] = 2\n\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\nnormalize(df)\nprint(df[\"z\"], df[\"y\"])\nprint(df[\"nope\"])\n",
        );

    // assert: both edits arrive as one new leg.
    assert_eq!(linter.dataframes_total, 2);
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'nope'"));
}

#[test]
fn test_should_follow_a_chain_of_delegating_helpers_through_self_and_await() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\nclass Job:\n    async def top(self, df):\n        await self.middle(df)\n\n    async def middle(self, df):\n        self.bottom(df)\n\n    def bottom(self, df):\n        df.drop(columns=[\"a\"], inplace=True)\n        df[\"z\"] = 1\n\n    async def run(self):\n        df = pd.read_csv(\"a.csv\", usecols=[\"a\", \"b\"])\n        await self.top(df)\n        print(df[\"z\"], df[\"b\"])\n        print(df[\"a\"])\n",
        );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'a'"));
}

#[test]
fn test_should_ignore_delegation_to_a_function_that_does_not_edit_the_frame() {
    // arrange / act
    let (linter, errors) = lint_source(
            "import pandas as pd\n\n\ndef show(df):\n    print(df)\n\n\ndef report(df):\n    show(df)\n    log(df)\n\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\nreport(df)\nprint(df[\"a\"])\n",
        );

    // assert
    assert_eq!(linter.dataframes_total, 1);
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_mark_unresolved_when_a_mutating_helper_is_called_conditionally() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\ndef _fix(df):\n    df[\"z\"] = 1\n\n\ndef normalize(df, flag):\n    if flag:\n        _fix(df)\n\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\nnormalize(df, True)\nprint(df[\"z\"])\n",
        );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
}

#[test]
fn test_should_not_loop_on_mutually_delegating_helpers() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\ndef ping(df):\n    df[\"z\"] = 1\n    pong(df)\n\n\ndef pong(df):\n    ping(df)\n\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\nping(df)\nprint(df[\"zzz\"])\n",
        );

    // assert: the cycle makes the columns unknown rather than hanging.
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
}

#[test]
fn test_should_not_apply_delegated_edits_made_after_the_parameter_is_copied() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\ndef _fix(df):\n    df[\"z\"] = 1\n\n\ndef normalize(df):\n    df = df.copy()\n    _fix(df)\n    return df\n\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\nnormalize(df)\nprint(df[\"z\"])\n",
        );

    // assert: the copy was edited, not the caller's frame.
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
}

#[test]
fn test_should_not_let_a_class_level_frame_leak_out_of_the_class_body() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\nclass C:\n    df = pd.read_csv(\"a.csv\", usecols=[\"a\"])\n\n\ndf = build()\nprint(df[\"zzz\"])\n",
        );

    // assert
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_not_show_a_class_level_frame_to_its_methods() {
    // arrange / act: `df` inside `run` is a global lookup, not the class attribute.
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\nclass C:\n    df = pd.read_csv(\"a.csv\", usecols=[\"a\"])\n\n    def run(self):\n        return df[\"zzz\"]\n",
        );

    // assert
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_still_check_a_class_level_frame_within_the_class_body() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\nclass C:\n    df = pd.read_csv(\"a.csv\", usecols=[\"a\"])\n    bad = df[\"zzz\"]\n",
        );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
}

#[test]
fn test_should_still_show_a_module_frame_to_methods_and_class_body() {
    // arrange / act
    let (_, errors) = lint_for_leg_tests(
        "class C:\n    bad = df[\"zzz\"]\n\n    def run(self):\n        return df[\"yyy\"]\n",
    );

    // assert
    assert_eq!(errors.len(), 2, "errors: {errors:?}");
}

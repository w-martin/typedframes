use super::*;

#[test]
fn test_should_follow_a_helper_that_returns_another_helpers_result() {
    // arrange / act
    let (linter, errors) = lint_source(
            "import pandas as pd\n\n\ndef _add(df):\n    df = df.copy()\n    df[\"z\"] = 1\n    return df\n\n\ndef normalize(df):\n    return _add(df)\n\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\nout = normalize(df)\nprint(out[\"z\"])\nprint(out[\"nope\"])\nprint(df[\"z\"])\n",
        );

    // assert: `out` has `z` (added on a copy), `df` does not.
    assert_eq!(linter.dataframes_total, 2);
    assert_eq!(errors.len(), 2, "errors: {errors:?}");
    assert!(errors[0].message.contains("'nope'"));
    assert!(errors[1].message.contains("'z'"));
}

#[test]
fn test_should_follow_a_method_that_returns_a_sibling_methods_result_through_await() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\nclass Job:\n    async def top(self, df):\n        return await self.bottom(df)\n\n    async def bottom(self, df):\n        df[\"z\"] = 1\n        return df\n\n    async def run(self):\n        df = pd.read_csv(\"a.csv\", usecols=[\"a\"])\n        df = await self.top(df)\n        print(df[\"z\"])\n        print(df[\"nope\"])\n",
        );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'nope'"));
}

#[test]
fn test_should_not_treat_returning_an_unknown_functions_result_as_a_passthrough() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\nfrom lib import validate\n\n\ndef check(df):\n    return validate(df)\n\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\ndf = check(df)\nprint(df[\"zzz\"])\n",
        );

    // assert: `validate` may return anything, so the columns are unknown.
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
}

#[test]
fn test_should_not_treat_returning_a_reshaping_helpers_result_as_a_passthrough() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\ndef _agg(df):\n    return df.groupby(\"a\").sum()\n\n\ndef top(df):\n    return _agg(df)\n\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\ndf = top(df)\nprint(df[\"zzz\"])\n",
        );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
}

#[test]
fn test_should_not_loop_on_helpers_that_return_each_other() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\ndef ping(df):\n    return pong(df)\n\n\ndef pong(df):\n    return ping(df)\n\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\ndf = ping(df)\nprint(df[\"zzz\"])\n",
        );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
}

#[test]
fn test_should_apply_a_global_rebinding_made_inside_a_method() {
    // arrange / act
    let (_, errors) = lint_for_leg_tests(
            "class C:\n    def refresh(self):\n        global df\n        df = df.pipe(g)\n\n\nprint(df[\"zzz\"])\n",
        );

    // assert: the module frame is unresolved after the method rebinds it.
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
}

#[test]
fn test_should_not_check_an_unannotated_receiver_against_another_functions_local_frame() {
    // arrange / act: `source` has no class annotation, so `load()` is not resolved,
    // and `df` must not be checked against the `df` local to `SqlDataSource.load`.
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\nclass SqlDataSource:\n    def load(self) -> pd.DataFrame:\n        df = pd.read_csv(\"data.csv\", usecols=[\"a\", \"b\"])\n        return df\n\n\ndef process(source) -> None:\n    df = source.load()\n    print(df[\"c\"])\n",
        );

    // assert
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_use_the_annotated_classs_own_concrete_method_over_other_classes() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\nclass A:\n    def load(self) -> pd.DataFrame:\n        df = pd.read_csv(\"a.csv\", usecols=[\"a\"])\n        return df\n\n\nclass B:\n    def load(self) -> pd.DataFrame:\n        df = pd.read_csv(\"b.csv\", usecols=[\"b\"])\n        return df\n\n\ndef process(src: A) -> None:\n    df = src.load()\n    print(df[\"a\"])\n    print(df[\"b\"])\n",
        );

    // assert: A's columns, not B's.
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'b'"));
    assert!(errors[0].message.contains("{a}"));
}

#[test]
fn test_should_leave_an_interface_call_open_when_implementations_disagree() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\nclass Source:\n    def load(self) -> pd.DataFrame: ...\n\n\nclass A(Source):\n    def load(self) -> pd.DataFrame:\n        df = pd.read_csv(\"a.csv\", usecols=[\"a\"])\n        return df\n\n\nclass B(Source):\n    def load(self) -> pd.DataFrame:\n        df = pd.read_csv(\"b.csv\", usecols=[\"b\"])\n        return df\n\n\ndef process(src: Source) -> None:\n    df = src.load()\n    print(df[\"anything\"])\n",
        );

    // assert: which implementation runs is unknown, so no column is claimed wrong.
    assert!(
        errors.iter().all(|e| e.code != CODE_UNKNOWN_COLUMN),
        "errors: {errors:?}"
    );
}

#[test]
fn test_should_accept_a_quoted_class_annotation_for_a_method_call_receiver() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\nclass A:\n    def load(self) -> pd.DataFrame:\n        df = pd.read_csv(\"a.csv\", usecols=[\"a\"])\n        return df\n\n\ndef process(src: \"A\") -> None:\n    df = src.load()\n    print(df[\"zzz\"])\n",
        );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
}

#[test]
fn test_should_not_resolve_a_method_the_annotated_class_does_not_define() {
    // arrange / act: `Source` has no `load`; only an unrelated class does.
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\nclass Source:\n    def other(self) -> int: ...\n\n\nclass A:\n    def load(self) -> pd.DataFrame:\n        df = pd.read_csv(\"a.csv\", usecols=[\"a\"])\n        return df\n\n\ndef process(src: Source) -> None:\n    df = src.load()\n    print(df[\"zzz\"])\n",
        );

    // assert
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_not_resolve_a_parameter_annotated_with_an_unknown_class() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\nclass A:\n    def load(self) -> pd.DataFrame:\n        df = pd.read_csv(\"a.csv\", usecols=[\"a\"])\n        return df\n\n\ndef process(src: ExternalClient) -> None:\n    df = src.load()\n    print(df[\"zzz\"])\n",
        );

    // assert
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_not_carry_a_parameter_class_out_of_its_function() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\nclass A:\n    def load(self) -> pd.DataFrame:\n        df = pd.read_csv(\"a.csv\", usecols=[\"a\"])\n        return df\n\n\ndef first(src: A) -> None:\n    pass\n\n\ndef second(src) -> None:\n    df = src.load()\n    print(df[\"zzz\"])\n",
        );

    // assert
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_follow_a_helper_that_rebinds_its_parameter_to_another_helpers_result() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\ndef _add(df):\n    df = df.copy()\n    df[\"z\"] = 1\n    return df\n\n\ndef normalize(df):\n    df = _add(df)\n    df[\"y\"] = 2\n    return df\n\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\ndf = normalize(df)\nprint(df[\"z\"], df[\"y\"])\nprint(df[\"nope\"])\n",
        );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'nope'"));
}

#[test]
fn test_should_treat_edits_after_a_delegating_rebind_as_possibly_in_place() {
    // arrange / act: `_id` may or may not copy, so whether `df["y"] = 2` reaches the
    // caller's frame is unknown.
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\ndef _id(df):\n    return df\n\n\ndef normalize(df):\n    df = _id(df)\n    df[\"y\"] = 2\n\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\nnormalize(df)\nprint(df[\"y\"])\n",
        );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
}

#[test]
fn test_should_not_follow_a_rebind_to_an_unknown_functions_result() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\nfrom lib import shape\n\n\ndef normalize(df):\n    df = shape(df)\n    return df\n\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\ndf = normalize(df)\nprint(df[\"zzz\"])\n",
        );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
}

#[test]
fn test_should_resolve_an_inherited_method_called_on_self() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\nclass Base:\n    def load(self) -> pd.DataFrame:\n        df = pd.read_csv(\"a.csv\", usecols=[\"a\"])\n        return df\n\n\nclass Child(Base):\n    def run(self):\n        df = self.load()\n        print(df[\"zzz\"])\n",
        );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
}

#[test]
fn test_should_prefer_an_overriding_method_over_the_inherited_one() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\nclass Base:\n    def load(self) -> pd.DataFrame:\n        df = pd.read_csv(\"a.csv\", usecols=[\"a\"])\n        return df\n\n\nclass Child(Base):\n    def load(self) -> pd.DataFrame:\n        df = pd.read_csv(\"b.csv\", usecols=[\"b\"])\n        return df\n\n    def run(self):\n        df = self.load()\n        print(df[\"b\"])\n        print(df[\"a\"])\n",
        );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'a'"));
}

#[test]
fn test_should_resolve_a_method_inherited_through_a_typed_parameter() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\nclass Base:\n    def load(self) -> pd.DataFrame:\n        df = pd.read_csv(\"a.csv\", usecols=[\"a\"])\n        return df\n\n\nclass Child(Base):\n    pass\n\n\ndef process(src: Child) -> None:\n    df = src.load()\n    print(df[\"zzz\"])\n",
        );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
}

#[test]
fn test_should_follow_an_inherited_passthrough_helper_and_inherited_delegate() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\nclass Base:\n    def _fix(self, df):\n        df[\"z\"] = 1\n\n    def _clean(self, df):\n        return df.dropna()\n\n\nclass Mid(Base):\n    def _norm(self, df):\n        self._fix(df)\n\n\nclass Child(Mid):\n    def run(self):\n        df = pd.read_csv(\"a.csv\", usecols=[\"a\"])\n        df = self._clean(df)\n        self._norm(df)\n        print(df[\"z\"])\n        print(df[\"nope\"])\n",
        );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'nope'"));
}

#[test]
fn test_should_not_loop_on_cyclic_inheritance() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\nclass A(B):\n    def run(self):\n        df = self.load()\n        return df\n\n\nclass B(A):\n    pass\n",
        );

    // assert
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_resolve_an_annotated_method_of_a_class_defined_later() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\nfrom typing import Annotated\nfrom typedframes import BaseSchema, Column\n\n\nclass S(BaseSchema):\n    a = Column(type=int)\n\n\ndef process(src: Later) -> None:\n    df = src.load()\n    print(df[\"zzz\"])\n\n\nclass Later:\n    def load(self) -> Annotated[pd.DataFrame, S]:\n        return pd.read_csv(\"a.csv\")\n",
        );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
}

#[test]
fn test_should_resolve_a_method_called_on_a_locally_constructed_instance() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\nclass Loader:\n    def load(self) -> pd.DataFrame:\n        df = pd.read_csv(\"a.csv\", usecols=[\"a\"])\n        return df\n\n\ndef main() -> None:\n    loader = Loader()\n    df = loader.load()\n    print(df[\"zzz\"])\n",
        );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
}

#[test]
fn test_should_see_a_module_level_instance_from_inside_a_function() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\nclass Loader:\n    def load(self) -> pd.DataFrame:\n        df = pd.read_csv(\"a.csv\", usecols=[\"a\"])\n        return df\n\n\nloader = Loader()\n\n\ndef main() -> None:\n    df = loader.load()\n    print(df[\"zzz\"])\n",
        );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
}

#[test]
fn test_should_forget_an_instance_when_its_name_is_reassigned_or_shadowed() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\nclass Loader:\n    def load(self) -> pd.DataFrame:\n        df = pd.read_csv(\"a.csv\", usecols=[\"a\"])\n        return df\n\n\ndef reassigned() -> None:\n    loader = Loader()\n    loader = make()\n    df = loader.load()\n    print(df[\"zzz\"])\n\n\nloader = Loader()\n\n\ndef shadowed(loader) -> None:\n    df = loader.load()\n    print(df[\"zzz\"])\n",
        );

    // assert
    assert!(errors.is_empty(), "errors: {errors:?}");
}

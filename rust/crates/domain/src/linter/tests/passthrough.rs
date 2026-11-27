use super::*;

#[test]
fn test_should_keep_the_schema_through_a_passthrough_helper_defined_earlier() {
    // arrange / act
    let (linter, errors) = lint_source(
            "import pandas as pd\n\n\ndef clean(d):\n    return d.dropna()\n\n\ndf = pd.read_csv(\"x.csv\", usecols=[\"a\"])\ndf = clean(df)\nprint(df[\"zzz\"])\n",
        );

    // assert: validated against the original columns, and still one origin.
    assert_eq!(linter.dataframes_total, 1);
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
}

#[test]
fn test_should_keep_the_schema_through_a_passthrough_helper_defined_later() {
    // arrange / act
    let (linter, errors) = lint_source(
            "import pandas as pd\n\n\ndef main():\n    df = pd.read_csv(\"x.csv\", usecols=[\"a\"])\n    df = clean(df)\n    print(df[\"zzz\"])\n\n\ndef clean(d):\n    return d\n",
        );

    // assert
    assert_eq!(linter.dataframes_total, 1);
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
}

#[test]
fn test_should_carry_the_schema_to_a_differently_named_passthrough_result() {
    // arrange / act
    let (linter, errors) = lint_source(
            "import pandas as pd\n\n\ndef clean(d):\n    return d.dropna()\n\n\ndf = pd.read_csv(\"x.csv\", usecols=[\"a\"])\ncleaned = clean(df)\nprint(cleaned[\"zzz\"])\n",
        );

    // assert: not counted again, and validated against `df`'s columns.
    assert_eq!(linter.dataframes_total, 1);
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
}

#[test]
fn test_should_resolve_a_passthrough_method_called_on_self() {
    // arrange / act
    let (linter, errors) = lint_source(
            "import pandas as pd\n\n\nclass Job:\n    def _clean(self, df):\n        return df.dropna()\n\n    def run(self):\n        df = pd.read_csv(\"x.csv\", usecols=[\"a\"])\n        df = self._clean(df)\n        print(df[\"zzz\"])\n",
        );

    // assert
    assert_eq!(linter.dataframes_total, 1);
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
}

#[test]
fn test_should_resolve_a_passthrough_method_defined_after_its_caller() {
    // arrange / act
    let (linter, errors) = lint_source(
            "import pandas as pd\n\n\nclass Job:\n    def run(self):\n        df = pd.read_csv(\"x.csv\", usecols=[\"a\"])\n        df = self._clean(df)\n        print(df[\"zzz\"])\n\n    def _clean(self, df):\n        return df.dropna()\n",
        );

    // assert
    assert_eq!(linter.dataframes_total, 1);
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
}

#[test]
fn test_should_not_treat_a_reshaping_helper_as_passthrough() {
    // arrange / act
    let (linter, errors) = lint_source(
            "import pandas as pd\n\n\ndef clean(d):\n    return d.groupby(\"a\").sum()\n\n\ndf = pd.read_csv(\"x.csv\", usecols=[\"a\"])\ndf = clean(df)\nprint(df[\"zzz\"])\n",
        );

    // assert: the helper changes columns, so it is an unresolved leg.
    assert_eq!(linter.dataframes_total, 2);
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
}

#[test]
fn test_should_bind_a_self_method_with_a_schema_return_annotation() {
    // arrange / act
    let (linter, errors) = lint_source(
            "import pandas as pd\nfrom typing import Annotated\nfrom typedframes import BaseSchema, Column\n\n\nclass S(BaseSchema):\n    a = Column(type=int)\n\n\nclass Job:\n    def load(self) -> Annotated[pd.DataFrame, S]:\n        return pd.read_csv(\"x.csv\")\n\n    def run(self):\n        df = self.load()\n        print(df[\"zzz\"])\n",
        );

    // assert
    assert_eq!(linter.dataframes_typed, 1);
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
}

#[test]
fn test_should_not_borrow_another_classs_passthrough_method() {
    // arrange / act: `Other.clean` is a pass-through, but `Job` has no `clean` of its
    // own (it could be inherited or mixed in), so the call is opaque.
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\nclass Other:\n    def clean(self, d):\n        return d\n\n\nclass Job:\n    def run(self):\n        df = pd.read_csv(\"x.csv\", usecols=[\"a\"])\n        df = self.clean(df)\n        print(df[\"zzz\"])\n",
        );

    // assert: unverifiable, not checked against `df`'s old columns.
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
}

#[test]
fn test_should_not_borrow_a_same_named_method_return_schema_from_another_class() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\nfrom typing import Annotated\nfrom typedframes import BaseSchema, Column\n\n\nclass S1(BaseSchema):\n    a = Column(type=int)\n\n\nclass A:\n    def load(self) -> Annotated[pd.DataFrame, S1]:\n        return pd.read_csv(\"x.csv\")\n\n\nclass B:\n    def load(self):\n        return pd.read_csv(\"x.csv\", usecols=[\"q\"])\n\n    def run(self):\n        df = self.load()\n        print(df[\"q\"])\n",
        );

    // assert: `B.load` is B's own, so `q` is a column of `df`.
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_not_match_a_library_function_to_a_passthrough_method_of_the_same_name() {
    // arrange / act: `clean` is imported, `Job.clean` is a pass-through method.
    let (_, errors) = lint_source(
            "import pandas as pd\nfrom lib import clean\n\n\nclass Job:\n    def clean(self, d):\n        return d\n\n\ndf = pd.read_csv(\"x.csv\", usecols=[\"a\"])\ndf = clean(df)\nprint(df[\"zzz\"])\n",
        );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
}

#[test]
fn test_should_apply_columns_a_helper_adds() {
    // arrange / act
    let (linter, errors) = lint_source(
            "import pandas as pd\n\n\ndef add(df):\n    df[\"z\"] = 1\n    return df\n\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\", \"b\"])\ndf = add(df)\nprint(df[\"z\"])\nprint(df[\"a\"])\nprint(df[\"nope\"])\n",
        );

    // assert: `z` now exists, `nope` still does not; the edit is one new leg.
    assert_eq!(linter.dataframes_total, 2);
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'nope'"));
}

#[test]
fn test_should_apply_columns_a_helper_removes_and_renames_in_order() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\ndef fix(df):\n    del df[\"b\"]\n    df.rename(columns={\"a\": \"x\"}, inplace=True)\n    df.columns = [\"p\", \"q\"]\n    df.insert(0, \"r\", 1)\n    df.pop(\"q\")\n    return df\n\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\", \"b\"])\nout = fix(df)\nprint(out[\"p\"], out[\"r\"])\nprint(out[\"q\"])\nprint(df[\"b\"])\n",
        );

    // assert: `out` is {p, r}, and since the helper edits its argument in place
    // `df` is too: `q` is gone from `out`, `b` is gone from `df`.
    assert_eq!(errors.len(), 2, "errors: {errors:?}");
    assert!(errors[0].message.contains("'q'"));
    assert!(errors[1].message.contains("'b'"));
}

#[test]
fn test_should_apply_a_columns_edit_made_by_a_helper_method_on_self() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\nclass Job:\n    def _add(self, df):\n        df[[\"z\", \"y\"]] = 1\n        return df\n\n    def run(self):\n        df = pd.read_csv(\"a.csv\", usecols=[\"a\"])\n        df = self._add(df)\n        print(df[\"z\"], df[\"y\"])\n",
        );

    // assert
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_mark_unresolved_after_a_helper_with_a_conditional_edit() {
    // arrange / act
    let (linter, errors) = lint_source(
            "import pandas as pd\n\n\ndef add(df, flag):\n    if flag:\n        df[\"z\"] = 1\n    return df\n\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\ndf = add(df, True)\nprint(df[\"z\"])\n",
        );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
    assert!(linter.leg_events[0].reason.contains("edits the columns"));
}

#[test]
fn test_should_mark_unresolved_after_a_helper_with_an_unmodelled_inplace_edit() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\ndef fix(df):\n    df.set_index(\"a\", inplace=True)\n    return df\n\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\ndf = fix(df)\nprint(df[\"zzz\"])\n",
        );

    // assert
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
}

#[test]
fn test_should_not_count_a_helper_edit_against_an_unresolved_frame_again() {
    // arrange / act
    let (linter, _) = lint_source(
            "import pandas as pd\n\n\ndef add(df):\n    df[\"z\"] = 1\n    return df\n\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\ndf = df.some_unknown()\ndf = add(df)\n",
        );

    // assert: one origin, one unresolved leg -- the helper does not add a third.
    assert_eq!(linter.dataframes_total, 2);
}

#[test]
fn test_should_apply_helper_edits_through_await() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\nasync def add(df):\n    df[\"z\"] = 1\n    return df\n\n\nasync def main():\n    df = pd.read_csv(\"a.csv\", usecols=[\"a\"])\n    df = await add(df)\n    print(df[\"z\"])\n",
        );

    // assert
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_record_why_tracking_ended() {
    // arrange / act
    let (linter, _) = lint_for_leg_tests(
        "df = df.some_unknown()\nseries = df[\"a\"]\ndf = df[\"a\"]\ndf, extra = split(df)\n",
    );

    // assert
    let outcomes: Vec<(&str, &str)> = linter
        .leg_events
        .iter()
        .map(|e| (e.var.as_str(), e.outcome.as_str()))
        .collect();
    assert_eq!(
        outcomes,
        vec![("df", "unresolved"), ("df", "untracked")],
        "events: {:?}",
        linter.leg_events
    );
    assert!(linter.leg_events[0].reason.contains("some_unknown"));
    assert!(linter.leg_events[1].reason.contains("Series or scalar"));
}

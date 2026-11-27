use super::*;

#[test]
fn test_should_not_report_a_column_missing_after_assign_on_an_untracked_parameter() {
    // arrange: `inst` is a parameter nothing is known about, so `.assign()` keeps
    // its unknown columns plus the new one; `C` is not a bug.
    let source = r#"
import pandas as pd


def f(inst: pd.DataFrame) -> pd.DataFrame:
    work = inst.assign(DPD=inst["A"] - inst["B"])
    return work[work["C"] > 0]
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_not_report_columns_read_inside_an_assign_that_rebinds_an_untracked_frame() {
    // arrange: the reads in the keyword values run before the rebinding.
    let source = r#"
import pandas as pd

app = pd.read_csv("a.csv")
app = app.assign(P=app.INCOME / (1 + app.KIDS), G=lambda d: d.CREDIT - d.GOODS)
print(app["P"])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert: only the untracked-dataframe warning for the bare read.
    assert!(
        errors.iter().all(|e| e.code != CODE_UNKNOWN_COLUMN),
        "errors: {errors:?}"
    );
}

#[test]
fn test_should_mark_unresolved_after_an_unknown_method_reassignment() {
    // arrange / act: `some_unknown` is not a method the checker models, so `df`
    // no longer has a column set anyone can vouch for.
    let (linter, errors) = lint_for_leg_tests("df = df.some_unknown()\nprint(df[\"zzz\"])\n");

    // assert: a resolved leg was lost, so it counts once as an unresolved origin,
    // and access is unverifiable rather than checked against the stale schema.
    assert_eq!(linter.dataframes_total, 2);
    assert_eq!(linter.dataframes_typed, 1);
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
}

#[test]
fn test_should_mark_unresolved_after_an_unknown_function_reassignment() {
    // arrange / act
    let (linter, errors) = lint_for_leg_tests("df = clean(df)\nprint(df[\"zzz\"])\n");

    // assert
    assert_eq!(linter.dataframes_total, 2);
    assert_eq!(linter.dataframes_typed, 1);
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
}

#[test]
fn test_should_count_a_chain_of_unknown_ops_as_one_unresolved_leg() {
    // arrange / act: the second op has nothing left to lose.
    let (linter, errors) =
        lint_for_leg_tests("df = clean(df)\ndf = clean_more(df)\nprint(df[\"zzz\"])\n");

    // assert
    assert_eq!(linter.dataframes_total, 2);
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
}

#[test]
fn test_should_untrack_a_name_reassigned_from_an_unrelated_source() {
    // arrange / act: `df` now holds whatever `load_other()` returns.
    let (linter, errors) = lint_for_leg_tests("df = load_other()\nprint(df[\"zzz\"])\n");

    // assert: no stale unknown-column, and no claim about the new value.
    assert_eq!(linter.dataframes_total, 1);
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_untrack_a_name_reassigned_to_a_series() {
    // arrange / act
    let (_, errors) = lint_for_leg_tests("df = df[\"a\"]\nprint(df[\"zzz\"])\n");

    // assert
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_untrack_a_name_reassigned_to_a_scalar_result() {
    // arrange / act
    let (_, errors) = lint_for_leg_tests("df = df.sum()\nprint(df[\"zzz\"])\n");

    // assert
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_keep_validating_after_schema_preserving_reassignments() {
    // arrange: ops that return a frame with the same columns must not lose the
    // leg -- `df["zzz"]` is still a real unknown column.
    for rhs in [
        "df[df[\"a\"] > 1]",
        "df.copy()",
        "df.drop_duplicates()",
        "df.astype({\"a\": \"int\"})",
        "df.loc[df[\"a\"] > 1]",
        "df.head().reset_index()",
    ] {
        // act
        let (linter, errors) = lint_for_leg_tests(&format!("df = {rhs}\nprint(df[\"zzz\"])\n"));

        // assert
        assert_eq!(linter.dataframes_total, 1, "{rhs}");
        assert_eq!(errors.len(), 1, "{rhs}: {errors:?}");
        assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN, "{rhs}");
    }
}

#[test]
fn test_should_not_touch_a_module_level_binding_from_a_function_local_reassignment() {
    // arrange / act: the function's `df` is a different variable.
    let (_, errors) = lint_for_leg_tests(
        "def helper():\n    df = load_other()\n    return df\n\nprint(df[\"zzz\"])\n",
    );

    // assert: the module-level `df` is still validated.
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
}

#[test]
fn test_should_untrack_rather_than_unresolve_a_call_to_a_helper_defined_later() {
    // arrange / act: `clean` is defined below its caller, so its return schema is
    // simply not inferred yet -- that is not evidence it is unknown.
    let source = "import pandas as pd\n\n\ndef main():\n    df = pd.read_csv(\"x.csv\", usecols=[\"a\"])\n    df = clean(df)\n    print(df[\"zzz\"])\n\n\ndef clean(d):\n    return d.groupby(\"a\").sum()\n";
    let mut linter = Linter::new();
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert: no unresolved leg counted, no false unverifiable-column.
    assert_eq!(linter.dataframes_total, 1);
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_untrack_names_rebound_by_tuple_unpacking() {
    // arrange / act
    let (_, errors) = lint_for_leg_tests("df, extra = split(df)\nprint(df[\"zzz\"])\n");

    // assert
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_end_a_stale_leg_for_an_annotated_reassignment() {
    // arrange / act
    let (linter, errors) = lint_for_leg_tests("df: pd.DataFrame = clean(df)\nprint(df[\"zzz\"])\n");

    // assert
    assert_eq!(linter.dataframes_total, 2);
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
}

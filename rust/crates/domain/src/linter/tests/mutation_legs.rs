use super::*;

#[test]
fn test_should_count_subscript_column_addition_as_a_new_leg() {
    // arrange: adding a genuinely new column via subscript assignment is a real
    // schema change -- its own new leg, same as rename/drop/assign (see
    // count_mutation_leg's doc comment), rather than mutating the old schema
    // object in place.
    let source = r#"
import pandas as pd

df = pd.read_csv("x.csv", usecols=["a", "b"])
df["c"] = 1
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(linter.dataframes_total, 2);
    assert_eq!(linter.dataframes_typed, 2);
}

#[test]
fn test_should_not_count_subscript_assignment_to_an_existing_column_as_a_new_leg() {
    // arrange: overwriting an EXISTING column's values doesn't change the schema
    // -- no new leg.
    let source = r#"
import pandas as pd

df = pd.read_csv("x.csv", usecols=["a", "b"])
df["a"] = 1
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(linter.dataframes_total, 1);
    assert_eq!(linter.dataframes_typed, 1);
}

#[test]
fn test_should_propagate_unresolved_base_through_subscript_mutation_without_false_positive() {
    // arrange: regression coverage for the same false-positive class fixed for
    // rename/drop -- an unresolved base's placeholder column list must not be
    // treated as a confirmed-empty schema.
    let source = r#"
import pandas as pd


def get_unresolved() -> pd.DataFrame:
    return pd.read_sql(some_dynamic_query, conn)


df = get_unresolved()
df["a"] = 1
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert: the write reads nothing, so there is nothing to call unverifiable, and
    // certainly no false unknown-column.
    assert!(
        errors.iter().all(|e| e.code == CODE_UNTRACKED_DATAFRAME),
        "errors: {errors:?}"
    );
    assert_eq!(linter.dataframes_total, 3);
    assert_eq!(linter.dataframes_typed, 0);
}

#[test]
fn test_should_count_pop_as_a_new_leg() {
    // arrange
    let source = r#"
import pandas as pd

df = pd.read_csv("x.csv", usecols=["a", "b"])
df.pop("a")
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(linter.dataframes_total, 2);
    assert_eq!(linter.dataframes_typed, 2);
}

#[test]
fn test_should_count_insert_as_a_new_leg() {
    // arrange
    let source = r#"
import pandas as pd

df = pd.read_csv("x.csv", usecols=["a", "b"])
df.insert(0, "c", 1)
"#;
    let mut linter = Linter::new();

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(linter.dataframes_total, 2);
    assert_eq!(linter.dataframes_typed, 2);
}

#[test]
fn test_should_propagate_unresolved_base_through_pop_without_false_positive() {
    // arrange: regression coverage -- pop() on an unresolved base used to report
    // a false "column doesn't exist" (see remove_column_inplace).
    let source = r#"
import pandas as pd


def get_unresolved() -> pd.DataFrame:
    return pd.read_sql(some_dynamic_query, conn)


df = get_unresolved()
df.pop("a")
print(df["b"])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert: only the expected unverifiable-column on the later access, no false
    // unknown-column from pop() itself.
    let unverifiable = errors
        .iter()
        .filter(|e| e.code == CODE_UNVERIFIABLE_COLUMN)
        .count();
    assert_eq!(unverifiable, 1, "errors: {errors:?}");
    assert!(errors.iter().all(|e| e.code != CODE_UNKNOWN_COLUMN));
    assert_eq!(linter.dataframes_total, 3);
    assert_eq!(linter.dataframes_typed, 0);
}

#[test]
fn test_should_track_columns_added_by_polars_with_columns() {
    // arrange / act
    let errors = lint_polars(
        "x = df.with_columns((pl.col(\"b\") + 1).alias(\"d\"), c=pl.col(\"a\") * 2)\n\
             print(x[\"c\"], x[\"d\"], x[\"a\"])\nprint(x[\"zzz\"])\n",
    );

    // assert: only the column nothing created is an error
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
    assert!(errors[0].message.contains("'zzz'"), "{errors:?}");
}

#[test]
fn test_should_not_guess_columns_for_an_unnamed_with_columns_expression() {
    // arrange / act: `pl.all() * 2` has no readable output name
    let errors = lint_polars("x = df.with_columns(pl.all() * 2)\nprint(x[\"zzz\"])\n");

    // assert: unverifiable, not a fabricated unknown-column
    assert!(
        errors.iter().all(|e| e.code != CODE_UNKNOWN_COLUMN),
        "errors: {errors:?}"
    );
}

#[test]
fn test_should_not_treat_pandas_dtype_keys_as_the_complete_column_set() {
    // arrange / act: `dtype=` overrides one column's type; `b` still exists in the file.
    let (_, errors) = lint_source(
            "import pandas as pd\ndf = pd.read_csv(\"x.csv\", dtype={\"a\": \"int64\"})\nprint(df[\"b\"])\n",
        );

    // assert: untracked-dataframe at most, never a fabricated unknown-column
    assert!(
        errors.iter().all(|e| e.code != CODE_UNKNOWN_COLUMN),
        "errors: {errors:?}"
    );
}

#[test]
fn test_should_treat_polars_schema_keys_as_the_complete_column_set() {
    // arrange / act
    let (_, errors) = lint_source(
            "import polars as pl\ndf = pl.read_csv(\"x.csv\", schema={\"a\": pl.Int64})\nprint(df[\"b\"])\n",
        );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
}

#[test]
fn test_should_follow_a_dict_comprehension_rename_over_known_columns() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\ndf = pd.read_csv(\"x.csv\", usecols=[\"Card ID\", \"Amount\"])\ndf = df.rename(columns={c: c.lower().replace(\" \", \"_\") for c in df.columns})\nprint(df[\"card_id\"], df[\"amount\"])\nprint(df[\"Amount\"])\n",
        );

    // assert: the renamed names resolve; the old one is gone
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
    assert!(errors[0].message.contains("'Amount'"), "{errors:?}");
}

#[test]
fn test_should_follow_a_list_comprehension_columns_assignment() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\ndf = pd.read_csv(\"x.csv\", usecols=[\"Card ID\", \"Amount\"])\ndf.columns = [c.lower().replace(\" \", \"_\") for c in df.columns]\nprint(df[\"card_id\"])\nprint(df[\"zzz\"])\n",
        );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'zzz'"), "{errors:?}");
}

#[test]
fn test_should_not_guess_a_rename_whose_mapping_calls_an_unknown_function() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\ndf = pd.read_csv(\"x.csv\", usecols=[\"Card ID\"])\ndf = df.rename(columns={c: fix(c) for c in df.columns})\nprint(df[\"anything\"])\n",
        );

    // assert: unverifiable, never a fabricated unknown-column
    assert!(
        errors.iter().all(|e| e.code != CODE_UNKNOWN_COLUMN),
        "errors: {errors:?}"
    );
}

#[test]
fn test_should_track_the_columns_of_a_polars_group_by_agg() {
    // arrange / act
    let errors = lint_polars_two_frames(
            "g = txn.group_by(\"card\").agg(pl.col(\"id\").count(), total=pl.col(\"amount\").sum())\nprint(g[\"card\"], g[\"total\"], g[\"id\"])\nprint(g[\"amount\"])\n",
        );

    // assert: keys and aggregates survive, the aggregated source column does not
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'amount'"), "{errors:?}");
}

#[test]
fn test_should_check_group_by_keys_and_agg_columns_against_the_frame() {
    // arrange / act
    let errors =
        lint_polars_two_frames("g = txn.group_by(\"cardd\").agg(total=pl.col(\"amoun\").sum())\n");

    // assert
    assert_eq!(errors.len(), 2, "errors: {errors:?}");
    assert!(errors.iter().all(|e| e.code == CODE_UNKNOWN_COLUMN));
}

#[test]
fn test_should_not_guess_the_columns_of_an_agg_with_an_unnamed_expression() {
    // arrange / act
    let errors = lint_polars_two_frames(
        "g = txn.group_by(\"card\").agg(pl.all().sum())\nprint(g[\"whatever\"])\n",
    );

    // assert
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_track_the_columns_of_a_polars_join() {
    // arrange / act
    let errors = lint_polars_two_frames(
            "j = txn.join(cards, on=\"card\")\nprint(j[\"id\"], j[\"limit\"], j[\"amount_right\"], j[\"card\"])\nprint(j[\"nope\"])\n",
        );

    // assert: the overlapping non-key column gets polars' default suffix
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'nope'"), "{errors:?}");
}

#[test]
fn test_should_check_polars_join_keys_and_keep_left_columns_for_a_semi_join() {
    // arrange / act
    let errors = lint_polars_two_frames(
            "j = txn.join(cards, left_on=\"card\", right_on=\"cardd\", how=\"semi\")\nprint(j[\"id\"])\nprint(j[\"limit\"])\n",
        );

    // assert: the bad right key and the right-only column are both caught
    assert_eq!(errors.len(), 2, "errors: {errors:?}");
    assert!(errors[0].message.contains("'cardd'"), "{errors:?}");
    assert!(errors[1].message.contains("'limit'"), "{errors:?}");
}

#[test]
fn test_should_leave_a_pandas_join_untracked() {
    // arrange / act: pandas joins on the index, so its columns aren't derived from `on=`
    let (_, errors) = lint_source(
            "import pandas as pd\na = pd.read_csv(\"a.csv\", usecols=[\"k\", \"x\"])\nb = pd.read_csv(\"b.csv\", usecols=[\"y\"])\nj = a.join(b, on=\"k\")\nprint(j[\"y\"], j[\"k\"])\n",
        );

    // assert
    assert!(errors.is_empty(), "errors: {errors:?}");
}

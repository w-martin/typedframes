use super::*;

#[test]
fn test_should_explain_a_with_columns_whose_output_names_cannot_be_read() {
    // arrange / act
    let (linter, errors) = lint_polars_source(
        "ex = [pl.col(\"a\").alias(\"c1\")]\nu = df.with_columns(ex)\nprint(u[\"a\"])\n",
    );

    // assert: known columns stay checkable-free, and the explain output names the argument
    assert!(errors.is_empty(), "errors: {errors:?}");
    assert!(
        linter
            .leg_events
            .iter()
            .any(|e| e.var == "u" && e.reason.contains("the variable `ex`")),
        "{:?}",
        linter
            .leg_events
            .iter()
            .map(|e| &e.reason)
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_should_keep_a_drop_made_after_a_columns_assignment_on_a_bare_load() {
    // arrange / act
    let (_, errors) = lint_source(
        "import pandas as pd\n\
             w = pd.read_csv(\"w.csv\")\n\
             w.columns = [c.strip().lower() for c in w.columns]\n\
             w.drop(columns=[\"snow\"], inplace=True)\n\
             print(w[\"snow\"])\n",
    );

    // assert
    assert!(
        errors
            .iter()
            .any(|e| e.code == CODE_UNKNOWN_COLUMN && e.message.contains("dropped")),
        "errors: {errors:?}"
    );
}

#[test]
fn test_should_keep_a_drop_through_a_boolean_mask_reassignment() {
    // arrange / act
    let (_, errors) = lint_source(
        "import pandas as pd\n\
             df = pd.read_csv(\"a.csv\")\n\
             df.drop(columns=[\"days\"], inplace=True)\n\
             df = df[df[\"flag\"].notna()]\n\
             print(df[\"days\"])\n",
    );

    // assert
    assert!(
        errors
            .iter()
            .any(|e| e.code == CODE_UNKNOWN_COLUMN && e.message.contains("dropped")),
        "errors: {errors:?}"
    );
}

#[test]
fn test_should_check_a_polars_drop_then_select_on_a_lazy_frame_parameter() {
    // arrange / act
    let (_, errors) = lint_source(
        "import polars as pl\n\n\n\
             def clean(lf: pl.LazyFrame) -> pl.LazyFrame:\n\
             \x20   lf = lf.drop(\"v\", \"w\")\n\
             \x20   return lf.select(\"id\", \"w\")\n",
    );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
    assert!(errors[0].message.contains("'w'"), "{errors:?}");
}

#[test]
fn test_should_check_a_polars_drop_then_use_on_a_frame_with_unknown_columns() {
    // arrange / act
    let (_, errors) = lint_source(
        "import polars as pl\n\
             df = pl.read_parquet(\"r.parquet\")\n\
             df = df.drop([\"current_a\"])\n\
             df.select(pl.col(\"current_a\").median())\n",
    );

    // assert
    assert!(
        errors
            .iter()
            .any(|e| e.code == CODE_UNKNOWN_COLUMN && e.message.contains("dropped")),
        "errors: {errors:?}"
    );
}

#[test]
fn test_should_require_the_polars_columns_a_function_reads_of_its_frame() {
    // arrange / act
    let (_, errors) = lint_source(
        "import polars as pl\n\n\n\
             def monthly(frame: pl.DataFrame) -> pl.DataFrame:\n\
             \x20   return frame.group_by(\"d\").agg(m=pl.col(\"ppss\").median())\n\n\n\
             monthly(pl.read_parquet(\"f.parquet\", columns=[\"d\", \"pps\"]))\n",
    );

    // assert
    assert!(
        errors
            .iter()
            .any(|e| e.code == CODE_MISSING_COLUMN && e.message.contains("{ppss}")),
        "errors: {errors:?}"
    );
}

#[test]
fn test_should_not_require_a_column_the_function_creates_itself() {
    // arrange / act
    let (_, errors) = lint_source(
            "import polars as pl\n\n\n\
             def f(frame: pl.DataFrame) -> pl.DataFrame:\n\
             \x20   return frame.with_columns(pl.col(\"a\").alias(\"b\")).select(pl.col(\"b\"))\n\n\n\
             f(pl.read_parquet(\"f.parquet\", columns=[\"a\"]))\n",
        );

    // assert
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_count_a_returned_load_as_an_origin() {
    // arrange / act
    let (linter, errors) = lint_source(
        "import pandas as pd\n\n\n\
             def read():\n\
             \x20   return pd.read_csv(\"a.csv\")\n",
    );

    // assert: the same warning and count as `df = pd.read_csv(..); return df`
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNTRACKED_DATAFRAME);
    assert_eq!(linter.untyped_sites.len(), 1);
}

#[test]
fn test_should_give_callers_the_literal_columns_of_a_returned_scan() {
    // arrange / act
    let (linter, errors) = lint_source(
        "import polars as pl\n\n\n\
             def read():\n\
             \x20   return pl.read_parquet(\"a.parquet\", columns=[\"a\", \"b\"])\n\n\n\
             lf = read()\nlf.select(\"zz\")\n",
    );

    // assert: counted once, at the call site, and checked there
    assert_eq!(linter.dataframes_typed, 1);
    assert!(
        errors.iter().any(|e| e.code == CODE_UNKNOWN_COLUMN),
        "errors: {errors:?}"
    );
}

#[test]
fn test_should_leave_a_function_with_a_declared_schema_return_alone() {
    // arrange / act
    let (_, errors) = lint_source(
        "import pandas as pd\nfrom typing import Annotated\n\n\
             class S(BaseSchema):\n    a = Column(int)\n\n\n\
             def read() -> Annotated[pd.DataFrame, S]:\n\
             \x20   return pd.read_csv(\"a.csv\")\n",
    );

    // assert
    assert!(
        errors.iter().all(|e| e.code != CODE_UNTRACKED_DATAFRAME),
        "errors: {errors:?}"
    );
}

#[test]
fn test_should_not_trust_dict_keys_when_the_columns_argument_is_not_a_literal() {
    // arrange / act
    let (_, errors) = lint_source(
        "import pandas as pd\n\n\n\
             def make(cols):\n\
             \x20   result = pd.DataFrame({\"a\": [1]}, columns=cols)\n\
             \x20   return result[\"b\"]\n",
    );

    // assert
    assert!(
        errors.iter().all(|e| e.code != CODE_UNKNOWN_COLUMN),
        "errors: {errors:?}"
    );
}

#[test]
fn test_should_not_warn_on_a_sibling_read_of_a_column_overwritten_in_place() {
    // arrange / act
    let (_, errors) = lint_source(
            "import polars as pl\nev = pl.read_parquet(\"e.parquet\")\n\
             ev = ev.with_columns(pl.col(\"d\").fill_null(0), (pl.col(\"d\") / 1000).alias(\"s\"))\n",
        );

    // assert
    assert!(
        errors.iter().all(|e| e.code != CODE_READ_BEFORE_ASSIGNMENT),
        "{errors:?}"
    );
}

#[test]
fn test_should_check_explode_and_over_partition_columns_on_a_known_frame() {
    // arrange / act
    let (_, errors) = lint_source(
        "import polars as pl\nev = pl.read_parquet(\"e.parquet\")\n\
             cats = ev.group_by(\"s\").agg(pl.col(\"c\").unique().alias(\"cats\"))\n\
             print(cats.explode(\"cat\"))\n\
             print(cats.with_columns(pl.col(\"cats\").len().over(\"ss\").alias(\"n\")))\n",
    );

    // assert
    let found = unknown_column_errors(&errors);
    assert!(
        found.iter().any(|(l, m)| *l == 4 && m.contains("'cat'")),
        "{errors:?}"
    );
    assert!(
        found.iter().any(|(l, m)| *l == 5 && m.contains("'ss'")),
        "{errors:?}"
    );
}

#[test]
fn test_should_report_an_old_name_read_after_a_rename_on_an_open_frame() {
    // arrange / act
    let (_, errors) = lint_source(
            "import polars as pl\nu = pl.read_csv(\"u.csv\")\n\
             u = u.rename({\"Signup Date\": \"signup\"}).with_columns(pl.col(\"Signup Date\").alias(\"d\"))\n",
        );

    // assert
    assert!(
        unknown_column_errors(&errors)
            .iter()
            .any(|(_, m)| m.contains("'Signup Date'")),
        "{errors:?}"
    );
}

#[test]
fn test_should_check_a_join_key_after_unique_when_pandas_is_also_imported() {
    // arrange / act
    let (_, errors) = lint_source(
        "import pandas as pd\nimport polars as pl\nev = pl.scan_parquet(\"e.parquet\")\n\
             prod = pl.read_parquet(\"p.parquet\")\nev = ev.unique(subset=[\"id\"])\n\
             out = ev.join(prod.lazy().select(\"pid\", \"brand\"), on=\"pidd\", how=\"left\")\n",
    );

    // assert
    assert!(
        unknown_column_errors(&errors)
            .iter()
            .any(|(_, m)| m.contains("'pidd'")),
        "{errors:?}"
    );
}

#[test]
fn test_should_check_a_polars_helper_against_the_known_frame_passed_to_it() {
    // arrange / act
    let (_, errors) = lint_source(
        "import polars as pl\n\n\ndef flags(d: pl.DataFrame) -> pl.DataFrame:\n\
             \x20   return d.with_columns((pl.col(\"cart_vlaue\") > 0).alias(\"has\"))\n\n\n\
             raw = pl.read_parquet(\"e.parquet\")\n\
             agg = raw.group_by(\"s\").agg(pl.col(\"p\").sum().alias(\"cart_value\"))\n\
             print(flags(agg).shape)\n",
    );

    // assert
    assert!(
        errors
            .iter()
            .any(|e| e.code == CODE_MISSING_COLUMN && e.message.contains("cart_vlaue")),
        "{errors:?}"
    );
}

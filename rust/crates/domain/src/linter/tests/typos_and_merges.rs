use super::*;

#[test]
fn test_should_resolve_string_enum_members_in_group_by_agg_and_drop() {
    // arrange
    let enum_def = "from enum import StrEnum\nimport polars as pl\n\n\n\
             class F(StrEnum):\n    S = \"s\"\n    C = \"c\"\n    P = \"p\"\n\n\n\
             raw = pl.read_parquet(\"e.parquet\")\n";

    // act
    let (_, agg) = lint_source(&format!(
        "{enum_def}out = raw.group_by(F.S).agg(pl.col(F.C).unique().alias(\"cats\"))\n\
             print(out.with_columns(pl.col(\"cts\").alias(\"n\")))\n"
    ));
    let (_, dropped) = lint_source(&format!(
        "{enum_def}out = raw.with_columns(pl.col(F.C).shift(1).alias(F.P))\nout = out.drop(F.P)\n\
             print(out.select(pl.col(F.P)))\n"
    ));
    let (_, member) = lint_source(&format!(
        "{enum_def}print(raw.group_by(\"s\").agg(pl.len().alias(F.NOPE)))\n"
    ));

    // assert
    assert!(
        unknown_column_errors(&agg)
            .iter()
            .any(|(_, m)| m.contains("'cts'")),
        "{agg:?}"
    );
    assert!(
        unknown_column_errors(&dropped)
            .iter()
            .any(|(_, m)| m.contains("'p'")),
        "{dropped:?}"
    );
    assert!(
        member
            .iter()
            .any(|e| e.message.contains("no member 'NOPE'")),
        "{member:?}"
    );
}

#[test]
fn test_should_report_a_later_typo_after_a_with_columns_typo_on_the_same_chain() {
    // arrange / act
    let (_, errors) = lint_source(
        "import polars as pl\nraw = pl.read_parquet(\"e.parquet\")\n\
             stats = raw.group_by(\"z\").agg(pl.len().alias(\"cnt\"))\n\
             stats = stats.with_columns(pl.col(\"cn\").cast(pl.Float64))\n\
             out = stats.to_pandas()\nout[\"cnt\"] = out[\"cn\"].round()\n",
    );

    // assert: both lines, once each
    let lines: Vec<usize> = unknown_column_errors(&errors)
        .iter()
        .map(|(l, _)| *l)
        .collect();
    assert_eq!(lines, vec![4, 6], "{errors:?}");
}

#[test]
fn test_should_report_one_typo_once_per_line() {
    // arrange / act
    let (_, errors) = lint_source(
        "import polars as pl\nraw = pl.read_parquet(\"e.parquet\")\n\
             agg = raw.group_by(\"s\").agg(pl.len().alias(\"n_events\"))\n\
             agg = agg.with_columns((pl.col(\"n_evnts\") + 1).alias(\"n_plus\"))\n",
    );

    // assert
    assert_eq!(unknown_column_errors(&errors).len(), 1, "{errors:?}");
}

#[test]
fn test_should_warn_on_a_read_of_a_column_a_later_merge_brings_in() {
    // arrange / act
    let (_, errors) = lint_source(
        "import pandas as pd\ndf = pd.read_csv(\"t.csv\")\nwx = pd.read_parquet(\"w.parquet\")\n\
             df[\"wet\"] = df[\"night\"] * df[\"precip\"]\n\
             df = df.merge(wx[[\"hour\", \"precip\"]], on=\"hour\", how=\"left\")\n",
    );

    // assert
    assert!(
        errors.iter().any(|e| e.code == CODE_READ_BEFORE_ASSIGNMENT
            && e.line == 4
            && e.message.contains("'precip'")),
        "{errors:?}"
    );
}

#[test]
fn test_should_not_warn_when_the_merge_brings_a_column_of_another_frame_bound_earlier() {
    // arrange / act
    let (_, errors) = lint_source(
        "import pandas as pd\ndf = pd.read_csv(\"a.csv\")\nprint(df[\"precip\"])\n\
             df = pd.read_csv(\"t.csv\")\nwx = pd.read_parquet(\"w.parquet\")\n\
             df = df.merge(wx[[\"hour\", \"precip\"]], on=\"hour\")\n\
             df2 = pd.read_csv(\"b.csv\")\nprint(df2[\"x\"])\n\
             df2 = df2.merge(wx[[\"hour\", \"x\"]], on=\"hour\", suffixes=(\"\", \"_r\"))\n",
    );

    // assert
    assert!(
        errors.iter().all(|e| e.code != CODE_READ_BEFORE_ASSIGNMENT),
        "{errors:?}"
    );
}

#[test]
fn test_should_explain_why_pivot_and_a_join_of_an_unknown_left_side_end_tracking() {
    // arrange / act
    let (linter, _) = lint_source(
        "import polars as pl\nraw = pl.read_parquet(\"e.parquet\")\n\
             long = raw.group_by(\"s\", \"t\").agg(pl.len().alias(\"n\"))\n\
             wide = long.pivot(on=\"t\", index=\"s\", values=\"n\").fill_null(0)\n\
             sess = pl.read_csv(\"s.csv\")\nagg = raw.group_by(\"s\").agg(pl.len().alias(\"n\"))\n\
             df = sess.join(agg, on=\"s\")\n",
    );

    // assert
    let reasons: Vec<&str> = linter
        .leg_events
        .iter()
        .map(|e| e.reason.as_str())
        .collect();
    assert!(
        reasons
            .iter()
            .any(|r| r.contains("`.pivot()` takes its new column names from the data")),
        "{reasons:?}"
    );
    let join: Vec<_> = reasons
        .iter()
        .filter(|r| r.contains("left side of `.join()`"))
        .collect();
    assert_eq!(join.len(), 1, "{reasons:?}");
}

#[test]
fn test_should_not_say_from_pandas_is_unmodelled_for_a_parameter_frame() {
    // arrange / act
    let (linter, _) = lint_source(
        "import pandas as pd\nimport polars as pl\n\n\ndef f(t: pd.DataFrame) -> pd.DataFrame:\n\
             \x20   frame = pl.from_pandas(t)\n\
             \x20   return frame.group_by(\"a\").agg(pl.len().alias(\"n\")).to_pandas()\n",
    );

    // assert
    assert!(
        linter
            .leg_events
            .iter()
            .all(|e| !e.reason.contains("from_pandas")),
        "{:?}",
        linter
            .leg_events
            .iter()
            .map(|e| &e.reason)
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_should_keep_usecols_advice_and_unverifiable_reads_out_of_tests() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\ndef helper() -> pd.DataFrame:\n    return load()\n\n\n\
             def test_trips():\n    raw = pd.read_csv(\"t.csv\")\n    assert raw[\"id\"].is_unique\n\
             \x20   h = helper()\n    assert h[\"a\"].all()\n",
        );

    // assert
    assert!(
        errors.iter().all(|e| e.code != CODE_UNVERIFIABLE_COLUMN),
        "{errors:?}"
    );
    assert!(
        errors
            .iter()
            .filter(|e| e.code == CODE_UNTRACKED_DATAFRAME)
            .all(|e| !e.message.contains("usecols") && e.message.contains("in a test")),
        "{errors:?}"
    );
}

#[test]
fn test_should_narrow_a_load_through_an_inplace_literal_rename() {
    // arrange / act
    let linter = suggest_for(
        "import pandas as pd\n\n\ndef run():\n    wx = pd.read_parquet(\"w.parquet\")\n\
             \x20   wx.rename(columns={\"obs\": \"hour\"}, inplace=True)\n\
             \x20   wx[\"hr\"] = wx[\"hour\"].dt.hour\n    print(wx[\"temp\"].mean())\n",
    );

    // assert
    assert_eq!(linter.suggestions.len(), 1, "{:?}", linter.withheld);
    assert_eq!(linter.suggestions[0].columns, vec!["obs", "temp"]);
}

#[test]
fn test_should_withhold_an_inplace_rename_onto_a_name_read_before_it() {
    // arrange / act
    let linter = suggest_for(
        "import pandas as pd\n\n\ndef run():\n    wx = pd.read_parquet(\"w.parquet\")\n\
             \x20   print(wx[\"hour\"])\n    wx.rename(columns={\"obs\": \"hour\"}, inplace=True)\n\
             \x20   print(wx[\"hour\"])\n",
    );

    // assert
    assert!(linter.suggestions.is_empty());
}

#[test]
fn test_should_narrow_a_polars_load_through_with_columns_filter_and_select() {
    // arrange / act
    let linter = suggest_for(
        "import polars as pl\n\n\ndef run():\n    s = pl.read_csv(\"s.csv\")\n\
             \x20   s = s.with_columns(pl.col(\"device\").str.to_lowercase())\n\
             \x20   s = s.filter(~pl.col(\"is_bot\"))\n    print(s.select(\"id\", \"device\"))\n",
    );

    // assert
    assert_eq!(linter.suggestions.len(), 1, "{:?}", linter.withheld);
    assert_eq!(
        linter.suggestions[0].columns,
        vec!["device", "is_bot", "id"]
    );
}

#[test]
fn test_should_withhold_a_polars_filter_over_every_column() {
    // arrange / act
    let linter = suggest_for(
            "import polars as pl\n\n\ndef run():\n    s = pl.read_csv(\"s.csv\")\n\
             \x20   s = s.filter(pl.all_horizontal(pl.all().is_not_null()))\n    print(s[\"id\"])\n",
        );

    // assert
    assert!(linter.suggestions.is_empty());
}

use super::*;

#[test]
fn test_should_know_the_columns_of_a_series_agg_list_and_a_named_reset_index() {
    // arrange
    let source = "import pandas as pd\n\ndf = pd.read_csv(\"f.csv\", usecols=[\"k\", \"v\"])\n\
            g = df.groupby(\"k\")[\"v\"].agg([\"mean\", \"sum\"])\nprint(g[\"mean\"], g[\"z1\"])\n\
            r = df.groupby(\"k\")[\"v\"].agg([\"max\"]).reset_index()\nprint(r[\"k\"], r[\"max\"])\n\
            n = df.groupby(\"k\")[\"v\"].nunique().reset_index(name=\"n\")\nprint(n[\"k\"], n[\"n\"], n[\"z2\"])\n\
            c = df.groupby(\"k\").size().reset_index(name=\"cnt\")\nprint(c[\"k\"], c[\"cnt\"])\n";

    // act / assert
    assert_eq!(unknown_names(source), ["z1", "z2"]);
}

#[test]
fn test_should_name_the_column_of_to_frame_and_keep_its_index_levels() {
    // arrange
    let source = "import pandas as pd\n\ndf = pd.read_csv(\"f.csv\", usecols=[\"k\", \"v\", \"t\"])\n\
            r = df.set_index(\"t\").resample(\"D\")[\"v\"].sum().to_frame()\nprint(r[\"v\"], r[\"z\"])\n\
            s = df.groupby(\"k\")[\"v\"].sum().to_frame()\nprint(s.merge(df, on=\"k\"))\n\
            s2 = s.reset_index()\nprint(s2[\"k\"], s2[\"v\"])\n";

    // act / assert
    assert_eq!(unknown_names(source), ["z"]);
}

#[test]
fn test_should_bring_back_index_levels_on_reset_index_of_a_named_index() {
    // arrange
    let source = "import pandas as pd\n\ndf = pd.read_csv(\"f.csv\", usecols=[\"k\", \"v\"])\n\
            a = df.set_index(\"k\")\nb = a.reset_index()\na = a.reset_index()\nprint(b[\"k\"], a[\"k\"], a[\"z\"])\n";

    // act / assert
    assert_eq!(unknown_names(source), ["z"]);
}

#[test]
fn test_should_keep_the_columns_through_a_row_mask_and_copy() {
    // arrange
    let source = "import pandas as pd\n\na = pd.read_csv(\"f.csv\", usecols=[\"r\", \"s\"])\n\
            b = a[a[\"r\"] > 3].copy()\nc = a[~a[\"r\"].isna() & (a[\"s\"] > 0)].copy()\nprint(b[\"s\"], b[\"z1\"], c[\"z2\"])\n";

    // act / assert
    assert_eq!(unknown_names(source), ["z1", "z2"]);
}

#[test]
fn test_should_follow_frames_in_a_dict_read_through_literal_keys() {
    // arrange
    let source = "import pandas as pd\n\nd = {\"one\": pd.read_csv(\"c.csv\", usecols=[\"p\"])}\n\
            d[\"one\"][\"new\"] = 1\nprint(d[\"one\"][\"p\"], d[\"one\"][\"new\"], d[\"one\"][\"z\"])\n";

    // act / assert
    assert_eq!(unknown_names(source), ["z"]);
}

#[test]
fn test_should_not_follow_a_dict_used_any_other_way() {
    // arrange
    let source = "import pandas as pd\n\nd = {\"one\": pd.read_csv(\"c.csv\", usecols=[\"p\"])}\n\
            d.update(one=pd.read_csv(\"e.csv\"))\nprint(d[\"one\"][\"z\"])\n";

    // act / assert
    assert!(unknown_names(source).is_empty());
}

#[test]
fn test_should_unpack_a_tuple_of_frames_a_local_function_returns() {
    // arrange
    let source = "import pandas as pd\n\n\ndef load():\n    a = pd.read_csv(\"a.csv\", usecols=[\"x\"])\n\
            \x20   b = pd.read_csv(\"b.csv\", usecols=[\"y\"])\n    return a, b\n\n\n\
            first, second = load()\nprint(first[\"x\"], second[\"y\"], first[\"z1\"], second[\"z2\"])\n";

    // act / assert
    assert_eq!(unknown_names(source), ["z1", "z2"]);
}

#[test]
fn test_should_resolve_polars_aliases_computed_from_loop_variables() {
    // arrange
    let source = "import polars as pl\n\ndf = pl.read_csv(\"f.csv\", columns=[\"a\", \"b\"])\n\
            out = df.with_columns([pl.col(c).alias(f\"{c}_x\") for c in [\"a\", \"b\"]])\n\
            for g in [\"a\", \"b\"]:\n    out = out.with_columns(pl.col(g).alias(\"g_\" + g))\n\
            print(out[\"a_x\"], out[\"g_b\"], out[\"z\"])\n";

    // act / assert
    assert_eq!(unknown_names(source), ["z"]);
}

#[test]
fn test_should_say_why_a_grouped_agg_with_function_lists_is_not_followed() {
    // arrange / act
    let (linter, _) = lint_source(
        "import pandas as pd\n\ndf = pd.read_csv(\"f.csv\", usecols=[\"k\", \"v\"])\n\
             h = df.groupby(\"k\").agg({\"v\": [\"min\", \"max\"]})\ns = df.eval(\"k + v\")\n",
    );

    // assert: the MultiIndex result is named, and a Series from eval is not a lost frame
    let rows: Vec<_> = linter
        .leg_events
        .iter()
        .map(|e| (e.var.as_str(), e.reason.as_str()))
        .collect();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].1.contains("MultiIndex"), "{rows:?}");
}

#[test]
fn test_should_keep_an_open_frame_rebound_in_a_loop_for_a_later_drop() {
    // arrange
    let source = "import polars as pl\n\ndf = pl.scan_parquet(\"t.parquet\")\n\
            for t in [\"a\", \"b\"]:\n    df = df.with_columns(pl.col(\"tags\").alias(\"tag_\" + t))\n\
            df = df.drop(\"created_at\")\ndf = df.with_columns(pl.col(\"created_at\").alias(\"z\"))\n";

    // act
    let errors = errors_for(source);

    // assert
    assert!(
        errors
            .iter()
            .any(|e| e.line == 7 && e.message.contains("dropped at line 6")),
        "{errors:?}"
    );
}

#[test]
fn test_should_report_a_read_after_a_rename_inside_the_same_loop_body_only() {
    // arrange
    let source = "import pandas as pd\n\nvalid = pd.read_csv(\"v.csv\")\nfor t in range(3):\n\
            \x20   valid = valid.rename(columns={\"target\": \"target_x\"})\n    print(valid[\"target\"])\n\
            print(valid[\"target\"])\n";

    // act
    let errors = errors_for(source);

    // assert: after the loop the frame may still be the one that has it
    let lines: Vec<_> = errors
        .iter()
        .filter(|e| e.code == "unknown-column")
        .map(|e| e.line)
        .collect();
    assert_eq!(lines, [6], "{errors:?}");
}

#[test]
fn test_should_treat_a_drop_with_columns_on_an_unknown_value_as_an_open_frame() {
    // arrange
    let source = "class Svc:\n    def run(self):\n        a = self.repo.read().drop_duplicates()\n\
            \x20       a = a.drop(columns=[\"year\"])\n        b = self.repo.read().rename(columns={\"x\": \"y\"})\n\
            \x20       return a[\"year\"], a[\"other\"], b[\"x\"], b[\"y\"]\n";

    // act
    let errors = errors_for(source);

    // assert
    let errs: Vec<_> = errors.iter().filter(|e| e.severity == "error").collect();
    assert_eq!(errs.len(), 2, "{errors:?}");
    assert!(errs[0].message.contains("'year'") && errs[1].message.contains("'x'"));
}

#[test]
fn test_should_warn_on_a_read_ahead_of_the_with_columns_that_creates_it() {
    // arrange
    let source = "import polars as pl\n\ndf = pl.scan_parquet(\"t.parquet\")\n_ = df[\"bucket\"]\n\
            df = df.with_columns(pl.col(\"h\").alias(\"bucket\"))\n_ = df[\"mrr\"]\n\
            df = df.with_columns(pl.col(\"mrr\").fill_null(0.0))\n_ = df[\"k\"]\ndf = df.assign(k=1)\n";

    // act
    let errors = errors_for(source);

    // assert
    let lines: Vec<_> = errors
        .iter()
        .filter(|e| e.code == CODE_READ_BEFORE_ASSIGNMENT)
        .map(|e| e.line)
        .collect();
    assert_eq!(lines, [4, 8], "{errors:?}");
}

#[test]
fn test_should_follow_a_frame_from_a_protocol_method_through_a_chained_call() {
    // arrange
    let source = "from typing import Protocol\n\nimport pandas as pd\n\n\n\
            class Repo(Protocol):\n    def read(self) -> pd.DataFrame: ...\n\n\n\
            class Svc:\n    def __init__(self, repo: Repo) -> None:\n        self.repo = repo\n\n\
            \x20   def run(self):\n        a = self.repo.read().drop_duplicates()\n        _ = a[\"age\"]\n\
            \x20       a[\"age\"] = 1\n        return a\n";

    // act
    let errors = errors_for(source);

    // assert
    assert!(
        errors
            .iter()
            .any(|e| e.code == CODE_READ_BEFORE_ASSIGNMENT && e.line == 16),
        "{errors:?}"
    );
}

#[test]
fn test_should_warn_on_a_likely_typo_named_nowhere_else() {
    // arrange
    let source = "import pandas as pd\n\ndf = pd.read_csv(\"a.csv\")\n\
            df[\"year_built\"] = df[\"year_buitl\"] + 1\nprint(df[[\"seats\", \"tial_num\"]], df[\"tail_num\"].sum(), df[\"tail_num\"])\n\
            print(df[\"sentiment\"], df[\"sentimernt\"])\n";

    // act
    let errors = errors_for(source);

    // assert: two names read once each, each the other's nearest, are left alone
    let typos: Vec<_> = errors
        .iter()
        .filter(|e| e.code == CODE_LIKELY_TYPO)
        .map(|e| (e.line, e.severity.as_str()))
        .collect();
    assert_eq!(typos, [(4, "warning"), (5, "warning")], "{errors:?}");
}

#[test]
fn test_should_not_warn_on_a_read_of_a_column_a_rename_created_before_a_later_write() {
    // arrange
    let source = "import pandas as pd\n\n\ndef f(frame: pd.DataFrame):\n\
            \x20   renamed = frame.rename(columns={\"C\": \"foo\"})\n    print(renamed[\"foo\"].sum())\n\
            \x20   renamed.loc[:, \"foo\"] = 1.0\n";

    // act
    let errors = errors_for(source);

    // assert
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn test_should_take_the_columns_of_a_multiindex_product_frame_from_its_names() {
    // arrange
    let source = "import pandas as pd\n\nw = pd.MultiIndex.from_product([[1], [2]], names=[\"station\", \"obs_date\"]).to_frame(index=False)\n\
            print(w[\"station\"], w[\"temp\"])\n";

    // act / assert
    assert_eq!(unknown_names(source), ["temp"]);
}

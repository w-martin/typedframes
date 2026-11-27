use super::*;

#[test]
fn test_should_record_where_a_derived_frame_stopped_being_tracked() {
    // arrange / act
    let (linter, _) = lint_source(
            "import pandas as pd\n\nimp = pd.read_parquet(\"i.parquet\")\nusers = pd.read_csv(\"u.csv\")\ndf = imp.merge(users, on=\"user_id\")\nclosed = pd.DataFrame({\"a\": [1]})\nwide = closed.pivot(columns=\"a\")\n",
        );

    // assert: both names appear in `--coverage-detail=explain`'s "tracking ended" rows
    let ended: Vec<_> = linter.leg_events.iter().map(|e| e.var.as_str()).collect();
    assert!(
        ended.contains(&"df") && ended.contains(&"wide"),
        "{:#?}",
        linter.leg_events
    );
}

#[test]
fn test_should_follow_an_open_frame_through_inplace_row_edits_and_row_masks() {
    // arrange / act
    let errors = without_untracked(open_frame_errors(
            "items.sort_values(\"a\", inplace=True)\nitems.reset_index(drop=True, inplace=True)\nboth = pd.get_dummies(items, columns=[\"cat\"])\nva = both[both[\"split\"] == \"va\"].drop(columns=\"split\")\nprint(va.loc[:, \"cat\"], va[\"split\"], va[\"cat_x\"])\n",
        ));

    // assert: `cat` was replaced by dummies, `split` dropped; `cat_x` is anyone's guess
    assert_eq!(
        codes_of(&errors),
        [("unknown-column", 8), ("unknown-column", 8)],
        "{errors:#?}"
    );
}

#[test]
fn test_should_treat_a_bare_dataframe_helper_result_as_an_open_frame() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\ndef normalise(frame: pd.DataFrame) -> pd.DataFrame:\n    return frame.copy()\n\n\ndef clean(raw: pd.DataFrame) -> pd.DataFrame:\n    imp = normalise(raw).drop_duplicates(\"id\")\n    imp[\"weekend\"] = imp[\"dow\"] >= 5\n    imp[\"dow\"] = imp[\"event_time\"].dt.day_of_week\n    return imp\n",
        );

    // assert: the read of `dow` on line 10 runs ahead of its only assignment
    let warned: Vec<_> = errors
        .iter()
        .filter(|e| e.code == "column-read-before-assignment")
        .collect();
    assert_eq!(warned.len(), 1, "{errors:#?}");
    assert_eq!(warned[0].line, 10);
}

#[test]
fn test_should_check_column_reads_in_every_expression_context() {
    // arrange: one typo per context, each on its own line
    let contexts = [
        "assert f[\"zz\"].sum() == 1\n",
        "assert f[\"a\"].sum() == 1, f[\"zz\"]\n",
        "z = 0\nz += f[\"zz\"].sum()\n",
        "d = {\"k\": f[\"zz\"]}\n",
        "y = f[\"zz\"] if True else 0\n",
        "h = lambda: f[\"zz\"]\n",
        "xs = [f[\"zz\"] for _ in range(2)]\n",
        "s = f\"{f['zz'].sum()}\"\n",
        "def g():\n    yield f[\"zz\"]\n",
    ];

    for context in contexts {
        // act
        let found = unknown_columns(&format!("{FRAME}{context}"));

        // assert
        assert_eq!(found.len(), 1, "{context:?}: {found:?}");
        assert!(found[0].1.contains("Column 'zz'"), "{context:?}");
    }
}

#[test]
fn test_should_not_check_a_lambda_or_comprehension_that_rebinds_the_frame_name() {
    // arrange / act
    let found = unknown_columns(&format!(
        "{FRAME}h = lambda f: f[\"zz\"]\nxs = [f[\"zz\"] for f in [1]]\n"
    ));

    // assert
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn test_should_check_merge_keys_against_both_operands() {
    // arrange / act
    let found = unknown_columns(
            "import pandas as pd\n\nf = pd.DataFrame({\"k\": [1], \"a\": [1]})\ng = pd.DataFrame({\"K\": [1], \"b\": [1]})\nm = f.merge(g, on=\"zz\")\nm2 = f.merge(g, left_on=\"k\", right_on=\"nope\")\nm3 = pd.merge(f, g, left_on=\"k\", right_on=\"K\")\n",
        );

    // assert: `on` is checked on both sides, `right_on` only on the right
    assert_eq!(found.len(), 3, "{found:?}");
    assert!(found.iter().all(|(line, _)| *line == 5 || *line == 6));
    assert!(found.iter().any(|(_, m)| m.contains("'nope'")));
}

#[test]
fn test_should_check_sort_dedupe_and_group_columns() {
    // arrange / act
    let found = unknown_columns(
            "import pandas as pd\nimport polars as pl\n\np = pd.DataFrame({\"a\": [1], \"b\": [2]})\nx = p.sort_values([\"a\", \"bb\"])\ny = p.drop_duplicates(subset=\"aa\")\nz = p.groupby(\"cc\")[\"a\"].count()\nq = pl.DataFrame({\"a\": [1], \"b\": [2]})\nu = q.sort([\"a\", \"bb\"])\nv = q.unique(subset=[\"aa\"])\n",
        );

    // assert
    let lines: Vec<usize> = found.iter().map(|(line, _)| *line).collect();
    assert_eq!(lines, [5, 6, 7, 9, 10], "{found:?}");
}

#[test]
fn test_should_not_read_an_index_level_as_a_missing_column() {
    // arrange / act
    let found = unknown_columns(
            "import pandas as pd\n\nf = pd.DataFrame({\"k\": [1], \"a\": [1]})\ng = f.set_index(\"k\")\nm = g.merge(f, on=\"k\")\ns = g.sort_values(\"k\")\nt = f.set_index(\"k\").groupby(\"k\")[\"a\"].sum()\n",
        );

    // assert
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn test_should_check_the_arguments_of_a_rebinding_assignment_against_the_frame_before_it() {
    // arrange / act
    let found = unknown_columns(
            "import pandas as pd\n\nacc = pd.DataFrame({\"plan\": [1], \"seats\": [2]})\ncat = pd.DataFrame({\"plan\": [1], \"price\": [1.0]})\nacc = acc.merge(cat, on=\"Plan\", how=\"left\")\n",
        );

    // assert: named the operands as they were, not the merged result
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(
        found.iter().all(|(_, m)| !m.contains("plan_x")),
        "{found:?}"
    );
}

#[test]
fn test_should_check_a_polars_chain_that_drops_or_renames_before_selecting() {
    // arrange / act
    let found = unknown_columns(
            "import polars as pl\n\nd = pl.DataFrame({\"policy_id\": [1], \"driver_age\": [2]})\na = d.rename({\"driver_age\": \"main_age\"}).select(\"policy_id\", \"driver_age\")\nb = d.filter(pl.col(\"driver_age\") > 1).drop(\"driver_age\").select(\"driver_age\")\n",
        );

    // assert
    let lines: Vec<usize> = found.iter().map(|(line, _)| *line).collect();
    assert_eq!(lines, [4, 5], "{found:?}");
}

#[test]
fn test_should_check_a_polars_select_in_return_position_against_a_class_constant() {
    // arrange / act
    let found = unknown_columns(
            "import polars as pl\n\n\nclass B:\n    KEEP = [\"policy_id\", \"driver_age\"]\n\n    def main(self) -> pl.DataFrame:\n        d = pl.read_csv(\"d.csv\", columns=[\"policy_id\", \"main_age\"])\n        return d.select(self.KEEP)\n",
        );

    // assert
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].1.contains("'driver_age'"));
}

#[test]
fn test_should_follow_a_load_call_and_a_merge_inside_a_chain() {
    // arrange / act
    let found = unknown_columns(
            "import pandas as pd\n\npm = pd.read_csv(\"p.csv\", usecols=[\"policy_id\", \"expiry_date\"])\npm = pm.drop(columns=[\"expiry_date\"])\ndf = pd.read_csv(\"c.csv\", usecols=[\"policy_id\", \"loss_date\"]).merge(pm, on=\"policy_id\")\ndf[\"d\"] = df[\"expiry_date\"] - df[\"loss_date\"]\n",
        );

    // assert
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].1.contains("'expiry_date'"));
}

#[test]
fn test_should_not_treat_a_helper_result_of_unknown_columns_as_empty_in_polars() {
    // arrange / act
    let (_, errors) = lint_source(
            "import polars as pl\n\n\ndef load() -> pl.DataFrame:\n    return pl.read_parquet(\"a.parquet\")\n\n\ndef use() -> pl.DataFrame:\n    scored = load()\n    return scored.select(\"a\", pl.col(\"b\")).with_columns(c=pl.col(\"a\"))\n",
        );

    // assert
    assert!(
        errors.iter().all(|e| e.code != CODE_UNKNOWN_COLUMN),
        "{errors:?}"
    );
}

#[test]
fn test_should_derive_a_helper_result_from_its_argument_in_polars() {
    // arrange / act
    let found = unknown_columns(
            "import polars as pl\n\n\ndef add_final(frame: pl.DataFrame) -> pl.DataFrame:\n    return frame.with_columns(p_final=pl.lit(1.0))\n\n\nclaims = pl.DataFrame({\"a\": [1]})\nscored = add_final(claims)\nok = scored.select(\"a\", \"p_final\")\nout = scored.select(\"a\", \"p_fnal\")\n",
        );

    // assert: `p_final` is derived from the body, the typo is the one error
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].1.contains("'p_fnal'") && found[0].1.contains("did you mean 'p_final'"));
}

#[test]
fn test_should_check_a_pandas_helper_that_returns_an_assign_chain() {
    // arrange / act
    let found = unknown_columns(
            "import pandas as pd\n\n\ndef add(frame: pd.DataFrame) -> pd.DataFrame:\n    return frame.assign(p=1).drop(columns=[\"b\"])\n\n\nf = pd.DataFrame({\"a\": [1], \"b\": [2]})\ng = add(f)\ny = g[\"b\"]\nz = g[\"p\"]\n",
        );

    // assert: `b` was dropped by the helper, `p` added
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].1.contains("'b'"));
}

#[test]
fn test_should_check_chains_on_an_annotated_polars_parameter() {
    // arrange / act
    let found = unknown_columns(
            "import polars as pl\n\n\ndef a(frame: pl.DataFrame) -> pl.DataFrame:\n    return frame.drop(\"x\").sort([\"id\", \"x\"])\n\n\ndef b(frame: pl.DataFrame) -> pl.DataFrame:\n    return frame.rename({\"x\": \"y\"}).select(\"x\", \"id\")\n",
        );

    // assert
    let lines: Vec<usize> = found.iter().map(|(line, _)| *line).collect();
    assert_eq!(lines, [5, 9], "{found:?}");
}

#[test]
fn test_should_check_polars_join_keys_against_a_method_result() {
    // arrange / act
    let found = unknown_columns(
            "import polars as pl\n\n\nclass B:\n    def agg(self, e: pl.DataFrame) -> pl.DataFrame:\n        return e.group_by(\"policy_id\").agg(n=pl.len())\n\n    def build(self, p: pl.DataFrame, e: pl.DataFrame) -> pl.DataFrame:\n        return p.join(self.agg(e), on=\"polcy_id\", how=\"left\")\n",
        );

    // assert
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].1.contains("'polcy_id'"));
}

#[test]
fn test_should_follow_melt_and_a_polars_agg_join_suffix() {
    // arrange / act
    let found = unknown_columns(
            "import pandas as pd\n\nf = pd.DataFrame({\"k\": [1], \"a\": [1]})\nf = f.melt(id_vars=\"k\")\nz = f[\"value\"] + f[\"a\"]\n",
        );

    // assert: the melted column is gone, `value` and `variable` exist
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].1.contains("'a'"));
}

#[test]
fn test_should_take_the_columns_of_a_polars_schema_list_in_return_position() {
    // arrange / act
    let found = unknown_columns(
            "from typing import Any\n\nimport polars as pl\n\n\nclass Repo:\n    def __init__(self, conn: Any) -> None:\n        self.conn = conn\n\n    def read(self) -> pl.DataFrame:\n        rows = self.conn.cursor().fetchall()\n        return pl.DataFrame(rows, schema=[\"policy_id\", \"region\"], orient=\"row\")\n\n    def use(self) -> pl.DataFrame:\n        df = self.read()\n        return df.select(\"policy_id\", \"regoin\")\n",
        );

    // assert
    assert_eq!(found.len(), 1, "{found:?}");
}

#[test]
fn test_should_not_carry_an_early_read_across_functions_into_a_read_before_assignment_warning() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\ndef a(frame: pd.DataFrame) -> pd.DataFrame:\n    out = frame.copy()\n    out[\"y\"] = out[\"tenure\"] * 2\n    return out\n\n\ndef b(panel: pd.DataFrame) -> pd.DataFrame:\n    out = panel.copy()\n    out[\"tenure\"] = 1\n    return out\n",
        );

    // assert
    assert!(
        errors
            .iter()
            .all(|e| e.code != "column-read-before-assignment"),
        "{errors:?}"
    );
}

#[test]
fn test_should_report_the_same_read_once_with_the_spelling_suggestion() {
    // arrange / act
    let found = unknown_columns(
            "import polars as pl\n\nd = pl.DataFrame({\"policy_id\": [1]})\nout = d.select(\"policy_idd\")\n",
        );

    // assert
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].1.contains("did you mean 'policy_id'"));
}

#[test]
fn test_should_follow_a_dict_aggregation_and_an_added_prefix() {
    // arrange / act
    let found = unknown_columns(
            "import pandas as pd\n\nf = pd.DataFrame({\"k\": [1], \"a\": [1], \"b\": [2]})\nr = f.groupby(\"k\").agg({\"a\": \"sum\", \"b\": \"mean\"})\ny = r[\"zz\"] + r[\"a\"]\np = f.add_prefix(\"p_\")\nz = p[\"a\"] + p[\"p_a\"]\n",
        );

    // assert
    let lines: Vec<usize> = found.iter().map(|(line, _)| *line).collect();
    assert_eq!(lines, [5, 7], "{found:?}");
}

#[test]
fn test_should_know_columns_written_under_a_loop_variable_name() {
    // arrange / act
    let (_, errors) = lint_source(
        "import pandas as pd\n\
             df = pd.read_csv(\"a.csv\", usecols=[\"a\", \"b\"])\n\
             for c in [\"a\", \"b\"]:\n    df[f\"{c}_log\"] = df[c]\n\
             print(df[\"a_log\"])\nprint(df[\"c_log\"])\n",
    );

    // assert: both loop-made names exist, a name no iteration makes does not
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'c_log'"), "{errors:?}");
}

#[test]
fn test_should_know_columns_written_under_an_enumerate_index_name() {
    // arrange / act
    let (_, errors) = lint_source(
        "import pandas as pd\nKW = [\"x\", \"y\"]\n\
             df = pd.read_csv(\"a.csv\", usecols=[\"d\"])\n\
             for i, k in enumerate(KW):\n    df[f\"kw_{i}\"] = df[\"d\"]\n\
             print(df[\"kw_1\"])\n",
    );

    // assert
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_open_a_frame_written_under_a_name_nothing_can_resolve() {
    // arrange / act
    let (linter, errors) = lint_source(
        "import pandas as pd\n\
             df = pd.read_csv(\"a.csv\", usecols=[\"a\"])\n\
             for c in get_cols():\n    df[f\"{c}_log\"] = df[\"a\"]\n\
             print(df[\"a\"])\nprint(df[\"x_log\"])\n",
    );

    // assert: never an error; the known column stays quiet, the other is unverifiable once
    assert!(
        errors.iter().all(|e| e.code != CODE_UNKNOWN_COLUMN),
        "{errors:?}"
    );
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'x_log'"), "{errors:?}");
    assert!(linter
        .leg_events
        .iter()
        .any(|e| e.reason.contains("computed at runtime")));
}

#[test]
fn test_should_open_a_frame_assigned_a_double_star_mapping() {
    // arrange / act
    let (_, errors) = lint_source(
        "import pandas as pd\n\
             df = pd.read_csv(\"a.csv\", usecols=[\"a\"])\n\
             df = df.assign(**build(df))\nprint(df[\"a_sq\"])\n",
    );

    // assert
    assert!(
        errors.iter().all(|e| e.code != CODE_UNKNOWN_COLUMN),
        "{errors:?}"
    );
}

#[test]
fn test_should_read_the_names_of_a_double_star_dict_literal_in_assign() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\
             df = pd.read_csv(\"a.csv\", usecols=[\"a\"])\n\
             df = df.assign(**{\"a_sq\": df[\"a\"] ** 2})\nprint(df[\"a_sq\"])\nprint(df[\"zz\"])\n",
        );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'zz'"), "{errors:?}");
}

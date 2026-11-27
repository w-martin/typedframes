use super::*;

#[test]
fn test_should_keep_tracking_a_frame_derived_under_a_new_name() {
    // arrange
    let derivations = [
        "a.copy()",
        "a[[\"x\"]].copy()",
        "a.astype(int)",
        "a.drop_duplicates(\"x\")",
        "a.groupby(\"x\").tail(1)",
        "a.loc[a[\"x\"] > 0]",
        "a.iloc[:2]",
        "a.set_index(\"x\", drop=False)",
        "a.sort_values(\"x\").reset_index(drop=True)",
        "a.query(\"x > 1\")",
    ];
    for derivation in derivations {
        // act
        let (_, errors) = lint_source(&format!(
                "import pandas as pd\n\na = pd.DataFrame({{\"x\": [1], \"y\": [2]}})\nb = {derivation}\nz = b[\"zz\"]\n"
            ));

        // assert
        assert_eq!(
            codes_of(&errors),
            [("unknown-column", 5)],
            "{derivation}: {errors:#?}"
        );
    }
}

#[test]
fn test_should_drop_the_index_column_on_set_index_under_a_new_name() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\na = pd.DataFrame({\"x\": [1], \"y\": [2]})\nb = a.set_index(\"x\")\nprint(b[\"y\"], b[\"x\"])\n",
        );

    // assert
    assert_eq!(codes_of(&errors), [("unknown-column", 5)], "{errors:#?}");
}

#[test]
fn test_should_resolve_a_helper_returning_a_copied_literal_selection() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\ndef pick(frame: pd.DataFrame) -> pd.DataFrame:\n    out = frame[[\"user_id\", \"u_prior_ctr\"]].copy()\n    return out\n\n\nraw = pd.read_parquet(\"f.parquet\")\nhist = pick(raw)\nprint(hist[\"u_prior_ctr\"], hist[\"u_gap_min\"])\n",
        );

    // assert: the selection is closed whatever the argument was
    assert_eq!(
        codes_of(&errors),
        [("untracked-dataframe", 9), ("unknown-column", 11)],
        "{errors:#?}"
    );
}

#[test]
fn test_should_check_a_column_list_selection_inside_a_call_argument() {
    // arrange / act
    let errors = closed_frame_errors(
            "COLS = [\"position\", \"hr\"]\nprint(df[[\"position\", \"hr\"]])\nprint(df[COLS])\ny = df[COLS]\nprint(df[[\"position\", \"hour\"]])\n",
        );

    // assert: each bad list is reported once, the good one never
    assert_eq!(
        codes_of(&errors),
        [
            ("unknown-column", 5),
            ("unknown-column", 6),
            ("unknown-column", 7)
        ],
        "{errors:#?}"
    );
}

#[test]
fn test_should_check_columns_read_in_an_assign_lambda() {
    // arrange / act
    let errors = closed_frame_errors(
            "out = df.assign(ratio=lambda d: d[\"click\"] / d[\"hr\"], again=lambda d: d[\"ratio\"])\nout2 = df.query(\"click > 0\").assign(z=lambda d: d[\"nope\"])\n",
        );

    // assert: `ratio` comes from the same call, `hr` and `nope` are typos
    assert_eq!(
        codes_of(&errors),
        [("unknown-column", 4), ("unknown-column", 5)],
        "{errors:#?}"
    );
    assert!(errors[0].message.contains("'hr'"));
}

#[test]
fn test_should_check_columns_read_in_a_row_apply_and_pipe_lambda() {
    // arrange / act
    let errors = closed_frame_errors(
            "a = df.apply(lambda r: r[\"click\"] + r[\"clik\"], axis=1)\nb = df.pipe(lambda d: d[\"hr\"])\nc = df.apply(lambda col: col.max())\n",
        );

    // assert: only the typos, and a column-wise apply binds no row
    assert_eq!(
        codes_of(&errors),
        [("unknown-column", 4), ("unknown-column", 5)],
        "{errors:#?}"
    );
}

#[test]
fn test_should_check_named_aggregation_sources_and_track_its_outputs() {
    // arrange / act
    let errors = closed_frame_errors(
            "seg = df.groupby(\"hour\", as_index=False).agg(n=(\"click\", \"sum\"), bad=(\"clik\", \"mean\"))\nprint(seg[\"n\"], seg[\"hour\"], seg[\"clik\"])\nidx = df.groupby(\"hour\").agg(n=(\"click\", \"sum\"))\nprint(idx[\"hour\"])\nprint(idx.reset_index()[\"hour\"])\n",
        );

    // assert: the source typo, the missing output, and the key that became the index
    assert_eq!(
        codes_of(&errors),
        [
            ("unknown-column", 4),
            ("unknown-column", 5),
            ("unknown-column", 7)
        ],
        "{errors:#?}"
    );
}

#[test]
fn test_should_check_pivot_table_and_melt_arguments_on_a_closed_frame() {
    // arrange / act
    let errors = closed_frame_errors(
            "rep = df.pivot_table(index=\"position\", values=[\"click\"], aggfunc=\"mean\")\nlong = rep.reset_index().melt(id_vars=\"postion\")\nbad = df.pivot_table(index=\"postion\", values=\"click\")\nok = df.melt(id_vars=[\"position\"], value_vars=[\"click\", \"hour\"])\nbad2 = df.melt(id_vars=[\"position\"], value_vars=[\"clik\"])\n",
        );

    // assert
    assert_eq!(
        codes_of(&errors),
        [
            ("unknown-column", 5),
            ("unknown-column", 6),
            ("unknown-column", 8)
        ],
        "{errors:#?}"
    );
    assert!(errors[0].message.contains("'postion'"));
}

#[test]
fn test_should_report_a_read_after_an_inplace_drop_on_an_open_frame() {
    // arrange / act
    let errors = without_untracked(open_frame_errors(
            "items.drop(columns=[\"created_at\"], inplace=True)\nprint(items[\"created_at\"].min(), items[\"price\"].max())\n",
        ));

    // assert: only the dropped name, the rest of the frame is unknown
    assert_eq!(codes_of(&errors), [("unknown-column", 5)], "{errors:#?}");
    assert!(errors[0].message.contains("dropped at line 4"));
}

#[test]
fn test_should_report_a_read_after_a_rebinding_drop_and_a_copy_chain() {
    // arrange / act
    let errors = without_untracked(open_frame_errors(
            "slim = items.copy().drop(columns=[\"a\"])\nitems = items.drop(columns=[\"b\"])\nprint(slim[\"a\"], items[\"b\"], items[\"a\"])\n",
        ));

    // assert: `a` is only gone from `slim`
    assert_eq!(
        codes_of(&errors),
        [("unknown-column", 6), ("unknown-column", 6)],
        "{errors:#?}"
    );
}

#[test]
fn test_should_report_the_old_name_after_a_rename_on_an_open_frame() {
    // arrange / act
    let errors = without_untracked(open_frame_errors(
            "items = items.rename(columns={\"quality_score\": \"qscore\"})\nitems[\"q_rank\"] = items[\"quality_score\"].rank()\nprint(items[\"qscore\"])\n",
        ));

    // assert
    assert_eq!(codes_of(&errors), [("unknown-column", 5)], "{errors:#?}");
    assert!(errors[0].message.contains("renamed to 'qscore'"));
}

#[test]
fn test_should_stop_reporting_a_dropped_name_once_it_is_assigned_again() {
    // arrange / act
    let errors = without_untracked(open_frame_errors(
        "items.drop(columns=[\"a\"], inplace=True)\nitems[\"a\"] = 1\nprint(items[\"a\"])\n",
    ));

    // assert
    assert!(errors.is_empty(), "{errors:#?}");
}

#[test]
fn test_should_not_claim_a_drop_that_only_happens_on_some_paths() {
    // arrange / act
    let errors = without_untracked(open_frame_errors(
        "if FLAG:\n    items.drop(columns=[\"a\"], inplace=True)\nprint(items[\"a\"])\n",
    ));

    // assert
    assert!(errors.is_empty(), "{errors:#?}");
}

#[test]
fn test_should_not_claim_a_dropped_name_after_the_frame_is_rebuilt() {
    // arrange / act
    let errors = without_untracked(open_frame_errors(
            "items.drop(columns=[\"a\"], inplace=True)\nitems = items.merge(other)\nprint(items[\"a\"])\nitems.drop(columns=[\"b\"], inplace=True)\nrestore(items)\nprint(items[\"b\"])\n",
        ));

    // assert: a merge or a helper may bring the column back
    assert!(errors.is_empty(), "{errors:#?}");
}

#[test]
fn test_should_report_a_dropped_name_on_a_dataframe_annotated_parameter() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\ndef f(frame: pd.DataFrame) -> pd.DataFrame:\n    out = frame.copy().drop(columns=[\"signup_date\"])\n    out[\"x\"] = out[\"signup_date\"].dt.year\n    return out\n",
        );

    // assert
    assert_eq!(codes_of(&errors), [("unknown-column", 6)], "{errors:#?}");
}

#[test]
fn test_should_warn_about_a_read_before_the_only_later_assignment() {
    // arrange / act
    let errors = without_untracked(open_frame_errors(
            "items[\"blend\"] = items[\"prior\"] * items[\"ctr\"]\nitems[\"prior\"] = items[\"clicks\"] / items[\"views\"]\nitems[\"ctr\"] = items[\"ctr\"].fillna(0)\n",
        ));

    // assert: `ctr` reads itself when it is written, so it was there already
    assert_eq!(
        codes_of(&errors),
        [("column-read-before-assignment", 4)],
        "{errors:#?}"
    );
    assert_eq!(errors[0].severity, "warning");
    assert!(errors[0].message.contains("line 5"));
}

#[test]
fn test_should_drop_columns_replaced_by_get_dummies() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\ndf = pd.DataFrame({\"user_id\": [1], \"category\": [\"a\"]})\nboth = pd.get_dummies(df, columns=[\"category\"], dtype=int)\nprint(both[\"category\"], both[\"category_a\"], both[\"user_id\"], df[\"category\"])\n",
        );

    // assert: only the replaced column; the dummies are not enumerable
    assert_eq!(codes_of(&errors), [("unknown-column", 5)], "{errors:#?}");
}

#[test]
fn test_should_check_a_column_list_constant_imported_from_another_file() {
    // arrange / act
    let (_, errors) = check_in_project(
            &[("cfg.py", "NUMERIC = [\"position\", \"hour\", \"quality_score\"]\n")],
            "import pandas as pd\n\nfrom cfg import NUMERIC\n\ndf = pd.DataFrame({\"position\": [1], \"hour\": [2], \"qscore\": [3.0]})\nx = df[NUMERIC]\n",
        );

    // assert
    assert_eq!(codes_of(&errors), [("unknown-column", 6)], "{errors:#?}");
    assert!(errors[0].message.contains("'quality_score'"));
}

#[test]
fn test_should_withhold_a_frame_once_however_many_columns_are_assigned_to_it() {
    // arrange
    let mut linter = Linter::new();
    linter.collect_suggestions = true;
    let source = "import pandas as pd\n\n\ndef norm(frame: pd.DataFrame) -> pd.DataFrame:\n    return frame.copy()\n\n\ndef clean(raw: pd.DataFrame) -> pd.DataFrame:\n    items = norm(raw)\n    items[\"a\"] = items[\"a\"].str.lower()\n    items[\"b\"] = items[\"b\"].fillna(0)\n    items[\"c\"] = items[\"a\"] + items[\"b\"].astype(str)\n    return items\n";

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert: one row for `items`, and never the made-up "not a plain assignment" reason
    let items: Vec<_> = linter
        .withheld
        .iter()
        .filter(|w| w.var == "items")
        .collect();
    assert_eq!(items.len(), 1, "{:#?}", linter.withheld);
    assert!(
        !items[0].reason.contains("plain `name"),
        "{}",
        items[0].reason
    );
}

#[test]
fn test_should_only_advise_final_for_a_constant_that_can_name_a_column() {
    // arrange
    let source = "import pandas as pd\n\n\nclass B:\n    SMOOTH = 20\n    KEY = \"user_id\"\n\n    def add(self, out: pd.DataFrame) -> pd.DataFrame:\n        out[\"a\"] = out[\"clicks\"] / (out[\"imps\"] + self.SMOOTH)\n        out[\"b\"] = out[self.KEY]\n        return out\n";
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert: the numeric tuning constant is not worth a notice, the key is
    let advised: Vec<_> = errors
        .iter()
        .filter(|e| e.code == "assumed-constant")
        .collect();
    assert_eq!(advised.len(), 1, "{advised:#?}");
    assert!(advised[0].message.contains("KEY"));
}

#[test]
fn test_should_name_the_origin_line_of_an_unresolved_frame_not_its_first_write() {
    // arrange
    let source = "import pandas as pd\n\n\ndef normalise(frame: pd.DataFrame) -> pd.DataFrame:\n    return frame.copy()\n\n\ndef clean(raw):\n    items = normalise(raw)\n    items[\"status\"] = items[\"status\"].str.lower()\n    return items\n";

    // act
    let (_, errors) = lint_source(source);

    // assert: the frame comes from line 9; nothing was "inferred", and no empty set is shown
    assert_eq!(errors.len(), 1, "{errors:#?}");
    assert!(
        errors[0].message.contains("the frame from line 9"),
        "{}",
        errors[0].message
    );
    assert!(
        !errors[0].message.contains("inferred column set"),
        "{}",
        errors[0].message
    );
    // not knowing is not the same as being wrong: it must not fail a plain `check`
    assert_eq!(errors[0].severity, "warning");
}

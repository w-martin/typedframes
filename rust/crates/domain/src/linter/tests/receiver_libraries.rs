use super::*;

#[test]
fn test_should_read_the_library_from_a_method_called_on_a_local_instance() {
    let source = format!(
            "{SERVICES}\n\ndef run():\n    sut = PandasService()\n    frame = sut.predict([1])\n    return frame[\"score\"].sum()\n"
        );

    let linter = suggest_for(&source);

    assert!(suggested_edit(&linter).contains("[[\"score\"]]"));
}

#[test]
fn test_should_read_the_library_from_a_method_called_on_an_annotated_parameter() {
    let source = format!(
            "{SERVICES}\n\ndef run(sut: PolarsService):\n    frame = sut.predict([1])\n    return frame[\"score\"].sum()\n"
        );

    let linter = suggest_for(&source);

    assert!(suggested_edit(&linter).contains(".select(\"score\")"));
}

#[test]
fn test_should_tell_two_classes_with_the_same_method_name_apart() {
    let source = format!(
            "{SERVICES}\n\ndef run(a: PandasService, b: PolarsService):\n    left = a.predict([1])\n    right = b.predict([2])\n    return left[\"x\"].sum() + right[\"y\"].sum()\n"
        );

    let linter = suggest_for(&source);

    assert_eq!(linter.suggestions.len(), 2, "{:?}", linter.withheld);
    assert!(linter.suggestions[0].edits[0]
        .replacement
        .contains("[[\"x\"]]"));
    assert!(linter.suggestions[1].edits[0]
        .replacement
        .contains(".select(\"y\")"));
}

#[test]
fn test_should_withhold_when_the_receiver_is_rebound() {
    let source = format!(
            "{SERVICES}\n\ndef run():\n    sut = PandasService()\n    sut = PandasService()\n    frame = sut.predict([1])\n    return frame[\"score\"].sum()\n"
        );

    let linter = suggest_for(&source);

    assert!(linter.suggestions.is_empty());
    let reason = &linter
        .withheld
        .iter()
        .find(|w| w.var == "frame")
        .unwrap()
        .reason;
    assert!(reason.contains("can't tell whether it is pandas or polars"));
    assert!(
        reason.contains("`predict` is annotated to return a DataFrame in a class of this file"),
        "{reason}"
    );
}

#[test]
fn test_should_not_resolve_a_receiver_rebound_by_a_walrus() {
    assert_eq!(
        suggestions_for_a_rebound_receiver("    if (sut := PolarsService()):\n"),
        0
    );
}

#[test]
fn test_should_not_resolve_a_receiver_rebound_by_a_walrus_in_a_comprehension() {
    assert_eq!(
        suggestions_for_a_rebound_receiver(
            "    [(sut := PolarsService()) for _ in other]\n    if True:\n"
        ),
        0
    );
}

#[test]
fn test_should_not_resolve_a_receiver_rebound_by_a_match_capture() {
    let source = format!(
            "{SERVICES}\n\ndef run(sut: PandasService, other):\n    match other:\n        case PolarsService() as sut:\n            pass\n    frame = sut.predict([1])\n    return frame[\"x\"].sum()\n"
        );

    assert!(suggest_for(&source).suggestions.is_empty());
}

#[test]
fn test_should_not_resolve_a_receiver_rebound_by_an_import() {
    assert_eq!(
        suggestions_for_a_rebound_receiver("    import os as sut\n    if True:\n"),
        0
    );
}

#[test]
fn test_should_not_resolve_a_receiver_rebound_by_a_loop_target() {
    assert_eq!(
        suggestions_for_a_rebound_receiver("    for sut in other:\n"),
        0
    );
}

#[test]
fn test_should_not_resolve_a_receiver_rebound_by_a_nested_nonlocal() {
    let source = format!(
            "{SERVICES}\n\ndef run():\n    sut = PandasService()\n\n    def swap():\n        nonlocal sut\n        sut = PolarsService()\n\n    swap()\n    frame = sut.predict([1])\n    return frame[\"x\"].sum()\n"
        );

    let linter = suggest_for(&source);

    assert!(linter.suggestions.is_empty());
}

#[test]
fn test_should_not_read_the_library_from_a_decorated_method() {
    let plain = "    def predict(self, rows) -> pd.DataFrame:\n        return pd.DataFrame(rows)\n";
    let decorated = "    @cache\n    def predict(self, rows) -> pd.DataFrame:\n        return pd.DataFrame(rows)\n";

    assert_eq!(suggestions_for_a_service_method(plain, ""), 1);
    assert_eq!(suggestions_for_a_service_method(decorated, ""), 0);
}

#[test]
fn test_should_still_read_the_library_from_a_static_method() {
    let body = "    @staticmethod\n    def predict(rows) -> pd.DataFrame:\n        return pd.DataFrame(rows)\n";

    assert_eq!(suggestions_for_a_service_method(body, ""), 1);
}

#[test]
fn test_should_not_read_the_library_from_an_async_method() {
    let body =
        "    async def predict(self, rows) -> pd.DataFrame:\n        return pd.DataFrame(rows)\n";

    assert_eq!(suggestions_for_a_service_method(body, ""), 0);
}

#[test]
fn test_should_not_read_the_library_when_a_subclass_overrides_the_method_differently() {
    let body = "    def predict(self, rows) -> pd.DataFrame:\n        return pd.DataFrame(rows)\n";
    let subclass = "class Fast(Service):\n    def predict(self, rows) -> pl.DataFrame:\n        return pl.DataFrame(rows)\n";
    let agreeing = "class Same(Service):\n    def predict(self, rows) -> pd.DataFrame:\n        return pd.DataFrame(rows)\n";

    assert_eq!(suggestions_for_a_service_method(body, subclass), 0);
    assert_eq!(suggestions_for_a_service_method(body, agreeing), 1);
}

#[test]
fn test_should_not_suggest_for_a_method_called_on_an_unknown_receiver() {
    let source = format!(
            "{SERVICES}\n\ndef run(sut):\n    frame = sut.predict([1])\n    return frame[\"score\"].sum()\n"
        );

    let linter = suggest_for(&source);

    assert!(linter.suggestions.is_empty());
}

#[test]
fn test_should_withhold_when_a_class_is_defined_twice_with_different_libraries() {
    let source = format!(
            "{SERVICES}\n\nclass PandasService:\n    def predict(self, rows) -> pl.DataFrame:\n        return pl.DataFrame(rows)\n\n\ndef run():\n    sut = PandasService()\n    frame = sut.predict([1])\n    return frame[\"score\"].sum()\n"
        );

    let linter = suggest_for(&source);

    assert!(linter.suggestions.is_empty());
    let caller = linter.withheld.iter().find(|w| w.var == "frame").unwrap();
    assert!(caller
        .reason
        .contains("can't tell whether it is pandas or polars"));
}

#[test]
fn test_should_read_the_library_of_a_frame_built_from_a_local() {
    // arrange
    let source = r#"
import pandas as pd


def load() -> pd.DataFrame:
    return pd.read_csv("x.csv")


def run():
    df = load()
    df["c"] = 1
    df = df.rename(columns={"a": "b"})
    out = df[df["b"] > 0]
    print(out["b"], out["c"])
"#;

    // act
    let linter = suggest_for(source);

    // assert
    assert!(
        linter
            .withheld
            .iter()
            .all(|w| !w.reason.contains("pandas or polars")),
        "{:?}",
        linter.withheld
    );
    assert!(linter.suggestions.iter().any(|s| s.var == "df"));
}

#[test]
fn test_should_not_read_the_library_of_a_local_bound_differently_on_two_paths() {
    // arrange
    let disagreeing = r#"
import pandas as pd
import polars as pl


def run(flag):
    if flag:
        df = pd.DataFrame({"a": [1]})
    else:
        df = pl.DataFrame({"a": [1]})
    out = df.rename(columns={"a": "b"})
    print(out["b"])
"#;
    let looped = r#"
import pandas as pd


def run(frames):
    for df in frames:
        out = df.rename(columns={"a": "b"})
        print(out["b"])
"#;

    for source in [disagreeing, looped] {
        // act
        let linter = suggest_for(source);

        // assert
        assert!(
            linter.suggestions.iter().all(|s| s.var != "out"),
            "{source}: {:?}",
            linter.suggestions
        );
    }
}

#[test]
fn test_should_withhold_when_a_helper_is_shadowed_by_a_parameter_or_rebound() {
    let source = r#"
import pandas as pd


def total(frame):
    return frame["a"].sum()


def run(total):
    df = pd.read_csv("x.csv")
    return total(df)
"#;
    let linter = suggest_for(source);

    assert!(linter.suggestions.is_empty());
    assert!(linter.withheld[0].reason.contains("passed to `total()`"));
}

#[test]
fn test_should_withhold_when_an_alias_of_the_frame_is_declared_global() {
    let source = r#"
import pandas as pd


def run():
    global KEPT
    df = pd.read_csv("x.csv")
    KEPT = df
    return df["a"]
"#;
    let linter = suggest_for(source);

    assert!(linter.suggestions.is_empty());
    assert!(linter.withheld[0].reason.contains("global"));
}

#[test]
fn test_should_ask_for_review_when_the_frame_is_printed() {
    let source = r#"
import pandas as pd


def run():
    df = pd.read_csv("x.csv")
    print(df)
    return df["a"]
"#;
    let linter = suggest_for(source);

    assert_eq!(linter.suggestions.len(), 1, "{:?}", linter.withheld);
    assert_eq!(linter.suggestions[0].tier, "review");
    assert!(linter.suggestions[0].notes[0].contains("displayed (line 7)"));
}

#[test]
fn test_should_treat_a_rebound_print_or_len_as_the_files_own_function() {
    // a function the file defines under a builtin's name is followed like any helper,
    // so the column it reads is counted rather than the call being assumed harmless
    for name in ["print", "len"] {
        let source = format!(
                "import pandas as pd\n\n\ndef {name}(x):\n    return x[\"zz\"]\n\n\ndef run():\n    df = pd.read_csv(\"x.csv\")\n    {name}(df)\n    return df[\"a\"]\n"
            );

        let linter = suggest_for(&source);

        assert_eq!(linter.suggestions.len(), 1, "{name}: {:?}", linter.withheld);
        assert_eq!(linter.suggestions[0].columns, vec!["zz", "a"], "{name}");
    }
}

#[test]
fn test_should_withhold_when_print_or_len_is_rebound_to_something_unknown() {
    for name in ["print", "len"] {
        let source = format!(
                "import pandas as pd\nfrom elsewhere import make\n\n{name} = make()\n\n\ndef run():\n    df = pd.read_csv(\"x.csv\")\n    {name}(df)\n    return df[\"a\"]\n"
            );

        let linter = suggest_for(&source);

        assert!(linter.suggestions.is_empty(), "{name}");
    }
}

#[test]
fn test_should_suggest_a_select_for_a_polars_load() {
    let linter =
        suggest_for("import polars as pl\ndf = pl.read_json(\"x.json\")\nprint(df[\"a\"])\n");
    assert!(linter.withheld.is_empty(), "{:?}", linter.withheld);
    assert!(linter.suggestions[0]
        .diff
        .contains("+df = df.select(\"a\")"));
}

#[test]
fn test_should_not_compute_suggestions_unless_asked() {
    // arrange / act: plain `check` mode.
    let mut linter = Linter::new();
    linter
        .check_file_internal(
            "import pandas as pd\ndf = pd.read_csv(\"x.csv\")\nprint(df[\"a\"])\n",
            Path::new("job.py"),
        )
        .unwrap();

    // assert
    assert!(linter.suggestions.is_empty());
    assert!(linter.withheld.is_empty());
}

#[test]
fn test_should_suggest_each_of_two_sites_independently() {
    let source = r#"
import pandas as pd


def a():
    x = pd.read_csv("a.csv")
    return x["p"]


def b():
    y = pd.read_csv("b.csv")
    helper(y)
    return y["q"]
"#;
    let linter = suggest_for(source);

    assert_eq!(linter.suggestions.len(), 1);
    assert_eq!(linter.suggestions[0].var, "x");
    assert_eq!(linter.withheld.len(), 1);
    assert_eq!(linter.withheld[0].var, "y");
}

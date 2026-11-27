use super::*;

#[test]
fn test_should_resolve_the_library_of_a_frame_returned_by_an_annotated_self_method() {
    // arrange
    let source = "import pandas as pd\n\n\nclass S:\n    def make(self) -> pd.DataFrame:\n        return pd.DataFrame({\"a\": [1]})\n\n    def use(self):\n        df = self.make()\n        return df[\"a\"]\n";

    // act
    let (_suggested, withheld) = untyped_site_outcome(source);

    // assert
    assert!(
        withheld.iter().all(|r| !r.contains("can't tell whether")),
        "{withheld:?}"
    );
}

#[test]
fn test_should_track_columns_named_by_a_class_constant_read_through_self() {
    // act
    let (typed, errors) = typed_count(SELF_ATTRIBUTE_COLUMNS);

    // assert
    assert_eq!(typed, 1);
    assert!(
        errors.iter().any(|e| e.code == CODE_UNKNOWN_COLUMN),
        "{errors:?}"
    );
}

#[test]
fn test_should_track_columns_named_by_a_class_constant_read_through_cls() {
    // arrange
    let source = "import pandas as pd\n\n\nclass C:\n    A = \"a\"\n\n    @classmethod\n    def f(cls):\n        out = pd.DataFrame(columns=[cls.A])\n        return out[\"zz\"]\n";

    // act
    let (typed, _errors) = typed_count(source);

    // assert
    assert_eq!(typed, 1);
}

#[test]
fn test_should_not_track_a_class_constant_a_subclass_in_the_file_rebinds() {
    // arrange: `self` in `f` may be a `Sub`, whose `A` is different.
    let source = format!("{SELF_ATTRIBUTE_COLUMNS}\n\nclass Sub(C):\n    A = \"b\"\n");

    // act
    let (typed, _errors) = typed_count(&source);

    // assert
    assert_eq!(typed, 0);
}

#[test]
fn test_should_not_track_a_class_constant_an_instance_assignment_may_replace() {
    // arrange
    let source = SELF_ATTRIBUTE_COLUMNS.replace(
        "    def f(self):",
        "    def reset(self):\n        self.A = \"q\"\n\n    def f(self):",
    );

    // act
    let (typed, _errors) = typed_count(&source);

    // assert
    assert_eq!(typed, 0);
}

#[test]
fn test_should_give_callers_the_columns_of_a_frame_a_function_returns_directly() {
    // arrange
    let source = "import pandas as pd\n\n\ndef make():\n    return pd.DataFrame({\"a\": [1], \"b\": [2]})\n\n\ndef use():\n    df = make()\n    return df[\"zz\"]\n";

    // act
    let (typed, errors) = typed_count(source);

    // assert: counted once, at the call site that resolves through `make`.
    assert_eq!(typed, 1);
    let error = errors
        .iter()
        .find(|e| e.code == CODE_UNKNOWN_COLUMN)
        .expect("an unknown-column error");
    assert!(error.message.contains("{a, b}"), "{}", error.message);
}

#[test]
fn test_should_resolve_a_returned_rename_of_a_resolved_frame() {
    // arrange
    let body = "    df = pd.read_sql(\"SELECT a, b FROM t\", conn)\n    return df.rename(columns={\"a\": \"x\"})\n";

    // act
    let message = returned_columns_message(body);

    // assert
    assert!(message.contains("{x, b}"), "{message}");
}

#[test]
fn test_should_resolve_a_returned_drop_and_column_selection() {
    // arrange
    let drop =
        "    df = pd.read_sql(\"SELECT a, b FROM t\", conn)\n    return df.drop(columns=[\"a\"])\n";
    let select =
        "    df = pd.read_sql(\"SELECT a, b FROM t\", conn)\n    return df[[\"a\"]].copy()\n";

    // act / assert
    assert!(returned_columns_message(drop).contains("{b}"));
    assert!(returned_columns_message(select).contains("{a}"));
}

#[test]
fn test_should_resolve_a_returned_chain_of_edits() {
    // arrange
    let body = "    df = pd.read_sql(\"SELECT a, b FROM t\", conn)\n    return df.rename(columns={\"a\": \"x\"}).assign(c=1).drop(columns=[\"b\"])\n";

    // act
    let message = returned_columns_message(body);

    // assert
    assert!(message.contains("{x, c}"), "{message}");
}

#[test]
fn test_should_not_read_a_positional_drop_or_rename_as_a_column_edit() {
    // arrange: in pandas these address row labels, so the columns are unchanged.
    let load = "    df = pd.read_sql(\"SELECT a, b FROM t\", conn)\n";
    let drop = format!("{load}    return df.drop([\"a\"])\n");
    let rename = format!("{load}    return df.rename({{\"a\": \"x\"}})\n");

    // act / assert
    assert!(!returned_columns_resolve(&drop));
    assert!(!returned_columns_resolve(&rename));
}

#[test]
fn test_should_resolve_a_drop_by_axis_and_a_reset_index_that_drops() {
    // arrange
    let load = "    df = pd.read_sql(\"SELECT a, b FROM t\", conn)\n";
    let by_axis = format!("{load}    return df.drop(\"a\", axis=1)\n");
    let reset = format!("{load}    return df.reset_index(drop=True)\n");

    // act / assert
    assert!(returned_columns_resolve(&by_axis));
    assert!(returned_columns_resolve(&reset));
}

#[test]
fn test_should_not_resolve_a_reset_index_that_adds_a_column_or_an_inplace_edit() {
    // arrange
    let load = "    df = pd.read_sql(\"SELECT a, b FROM t\", conn)\n";
    let reset = format!("{load}    return df.reset_index()\n");
    let inplace = format!("{load}    return df.rename(columns={{\"a\": \"x\"}}, inplace=True)\n");

    // act / assert
    assert!(!returned_columns_resolve(&reset));
    assert!(!returned_columns_resolve(&inplace));
}

#[test]
fn test_should_resolve_a_returned_cursor_fetch() {
    // arrange
    let body = "    cur = conn.cursor()\n    cur.execute(\"SELECT a, b FROM t\")\n    return cur.fetch_pandas_all()\n";

    // act
    let message = returned_columns_message(body);

    // assert
    assert!(message.contains("{a, b}"), "{message}");
}

#[test]
fn test_should_resolve_a_returned_snowpark_to_pandas() {
    // arrange
    let body = "    return sess.sql(\"SELECT a, b FROM t\").to_pandas()\n";

    // act
    let message = returned_columns_message(body);

    // assert
    assert!(message.contains("{a, b}"), "{message}");
}

#[test]
fn test_should_resolve_columns_for_an_assigned_snowpark_to_pandas() {
    // arrange
    let source = "df = session.sql(\"SELECT a, b FROM t\").to_pandas()\nprint(df[\"missing\"])\n";

    // act
    let errors = Linter::new()
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].message.contains("missing"));
}

#[test]
fn test_should_keep_a_returned_edit_of_an_unresolvable_frame_unresolved() {
    // arrange: dynamic SQL cannot be resolved, so neither can a rename of it.
    let source = "import pandas as pd\n\n\ndef make(conn, cols) -> pd.DataFrame:\n    df = pd.read_sql(f\"SELECT {cols} FROM t\", conn)\n    return df.rename(columns={\"a\": \"x\"})\n\n\ndef use(conn, cols):\n    df = make(conn, cols)\n    return df[\"zz\"]\n";

    // act
    let (_typed, errors) = typed_count(source);

    // assert
    assert!(
        errors.iter().any(|e| e.code == CODE_UNVERIFIABLE_COLUMN),
        "{errors:?}"
    );
    assert!(errors.iter().all(|e| e.code != CODE_UNKNOWN_COLUMN));
}

#[test]
fn test_should_give_callers_the_usecols_of_a_loader_a_function_returns_directly() {
    // arrange
    let source = "import pandas as pd\n\n\ndef load():\n    return pd.read_csv(\"x.csv\", usecols=[\"a\", \"b\"])\n\n\ndef use():\n    df = load()\n    return df[\"zz\"]\n";

    // act
    let (typed, errors) = typed_count(source);

    // assert
    assert_eq!(typed, 1);
    assert!(
        errors.iter().any(|e| e.code == CODE_UNKNOWN_COLUMN),
        "{errors:?}"
    );
}

#[test]
fn test_should_not_infer_columns_when_a_function_returns_in_more_than_one_place() {
    // arrange: either frame may come back.
    let source = "import pandas as pd\n\n\ndef make(flag):\n    if flag:\n        return pd.DataFrame({\"a\": [1]})\n    return pd.DataFrame({\"b\": [1]})\n\n\ndef use():\n    df = make(True)\n    return df[\"a\"]\n";

    // act
    let (typed, errors) = typed_count(source);

    // assert
    assert_eq!(typed, 0);
    assert!(
        errors.iter().all(|e| e.code != CODE_UNKNOWN_COLUMN),
        "{errors:?}"
    );
}

#[test]
fn test_should_prefer_columns_read_from_the_body_to_a_bare_frame_annotation() {
    // arrange
    let source = "import pandas as pd\n\n\ndef make() -> pd.DataFrame:\n    return pd.DataFrame({\"a\": [1]})\n\n\ndef use():\n    df = make()\n    return df[\"zz\"]\n";

    // act
    let (typed, errors) = typed_count(source);

    // assert
    assert_eq!(typed, 1);
    assert!(
        errors.iter().any(|e| e.code == CODE_UNKNOWN_COLUMN),
        "{errors:?}"
    );
}

#[test]
fn test_should_advise_declaring_final_for_a_class_constant_read_through_self() {
    // arrange: read twice, advised once.
    let source = format!(
        "{}        again = pd.DataFrame(columns=[self.A])\n",
        SELF_ATTRIBUTE_COLUMNS.replace("        return out[\"zz\"]\n", "")
    );

    // act
    let (_typed, errors) = typed_count(&source);

    // assert
    let advice: Vec<&LintError> = errors
        .iter()
        .filter(|e| e.code == CODE_ASSUMED_CONSTANT)
        .collect();
    assert_eq!(advice.len(), 1, "{errors:?}");
    assert_eq!(advice[0].severity, "info");
    assert!(
        advice[0].message.contains("`A: Final = ...`"),
        "{}",
        advice[0].message
    );
}

#[test]
fn test_should_not_advise_declaring_final_for_a_constant_already_final() {
    // arrange
    let source = SELF_ATTRIBUTE_COLUMNS
        .replace("    A = \"a\"", "    A: Final = \"a\"")
        .replace(
            "import pandas as pd",
            "from typing import Final\n\nimport pandas as pd",
        );

    // act
    let (typed, errors) = typed_count(&source);

    // assert: still resolved, and nothing to advise.
    assert_eq!(typed, 1);
    assert!(
        errors.iter().all(|e| e.code != CODE_ASSUMED_CONSTANT),
        "{errors:?}"
    );
}

#[test]
fn test_should_note_that_a_suggestion_through_a_self_method_assumes_it_is_not_overridden() {
    // arrange
    let source = "import pandas as pd\n\n\nclass S:\n    def reads(self, frame):\n        return frame[\"b\"]\n\n    def use(self):\n        df = pd.read_csv(\"x.csv\")\n        return self.reads(df)\n";
    let mut linter = Linter::new();
    linter.collect_suggestions = true;

    // act
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    let suggestion = linter.suggestions.first().expect("a suggestion");
    assert_eq!(suggestion.tier, "review");
    assert!(
        suggestion
            .notes
            .iter()
            .any(|n| n.contains("`self.reads()`")),
        "{:?}",
        suggestion.notes
    );
}

#[test]
fn test_should_not_infer_a_returned_frames_columns_from_a_non_literal_columns_argument() {
    // arrange: the data names `a` but `columns=` decides what the frame has.
    let source = "import pandas as pd\n\n\ndef make(cols):\n    return pd.DataFrame({\"a\": [1]}, columns=cols)\n\n\ndef use():\n    df = make([\"a\", \"b\"])\n    return df[\"b\"]\n";

    // act
    let (typed, errors) = typed_count(source);

    // assert
    assert_eq!(typed, 0);
    assert!(
        errors.iter().all(|e| e.code != CODE_UNKNOWN_COLUMN),
        "{errors:?}"
    );
}

#[test]
fn test_should_not_track_an_empty_placeholder_constant_read_through_self() {
    // arrange: a subclass elsewhere is expected to fill it in.
    let source = SELF_ATTRIBUTE_COLUMNS
        .replace("A = \"a\"", "A: list = []")
        .replace("columns=[self.A]", "columns=self.A");

    // act
    let (typed, errors) = typed_count(&source);

    // assert
    assert_eq!(typed, 0);
    assert!(
        errors.iter().all(|e| e.code != CODE_UNKNOWN_COLUMN),
        "{errors:?}"
    );
}

#[test]
fn test_should_not_track_a_class_constant_when_the_class_sets_attributes_dynamically() {
    // arrange
    for dynamic in [
        "    def set(self, name):\n        setattr(self, name, 1)\n\n",
        "    def load(self, values):\n        self.__dict__.update(values)\n\n",
        "    def __setattr__(self, name, value):\n        pass\n\n",
    ] {
        let source = SELF_ATTRIBUTE_COLUMNS
            .replace("    def f(self):", &format!("{dynamic}    def f(self):"));

        // act
        let (typed, _errors) = typed_count(&source);

        // assert
        assert_eq!(typed, 0, "{dynamic}");
    }
}

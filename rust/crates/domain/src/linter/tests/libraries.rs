use super::*;

#[test]
fn test_should_not_suggest_narrowing_inside_a_test_function() {
    // arrange
    let source = "import pandas as pd\n\n\ndef test_total():\n    df = pd.read_json(\"x.json\")\n    assert df[\"a\"].sum() > 0\n\n\ndef total():\n    df = pd.read_json(\"x.json\")\n    return df[\"a\"].sum()\n";
    let mut linter = Linter::new();
    linter.collect_suggestions = true;

    // act
    linter
        .check_file_internal(source, Path::new("test_mod.py"))
        .unwrap();

    // assert: only the non-test function is narrowed
    assert_eq!(linter.suggestions.len(), 1, "{:?}", linter.suggestions);
    assert_eq!(linter.suggestions[0].line, 10);
    assert!(
        linter
            .withheld
            .iter()
            .any(|w| w.line == 5 && w.reason.contains("inside a test")),
        "{:?}",
        linter.withheld
    );
}

#[test]
fn test_should_take_a_models_fields_as_the_columns_of_a_one_row_frame() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\nfrom pydantic import BaseModel\n\n\nclass Txn(BaseModel):\n    card_id: int\n    amount: float = 0.0\n\n\ndef score(txn: Txn):\n    frame = pd.DataFrame([txn.model_dump()])\n    print(frame[\"amount\"])\n    print(frame[\"amout\"])\n",
        );

    // assert: no untracked-dataframe noise, and the typo is caught
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
}

#[test]
fn test_should_keep_warning_for_a_one_row_frame_from_a_plain_dict_parameter() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\ndef score(payload: dict):\n    row = pd.DataFrame([payload])\n    return row\n",
        );

    // assert: a dict's keys are not known, so the warning stays
    assert!(
        errors.iter().any(|e| e.code == CODE_UNTRACKED_DATAFRAME),
        "{errors:?}"
    );
}

#[test]
fn test_should_resolve_a_call_through_a_protocol_typed_constructor_parameter() {
    // arrange: `self.store` is only known through the `Store` annotation on `__init__`
    let source = r#"
from typing import Protocol

import pandas as pd


class Store(Protocol):
    def read(self) -> pd.DataFrame: ...


class UseCase:
    def __init__(self, store: Store) -> None:
        self.store = store

    def run(self) -> float:
        frame = self.store.read()
        return frame["qty"].mean()
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert: the frame is recognised (counted, unresolved), so reading it says so
    assert_eq!(linter.dataframes_total, 1);
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
}

#[test]
fn test_should_suggest_for_a_frame_from_an_injected_dependency() {
    // arrange
    let source = "import pandas as pd\n\n\nclass Reader:\n    def read(self) -> pd.DataFrame:\n        return pd.read_csv(\"x.csv\")\n\n\nclass UseCase:\n    def __init__(self, reader: Reader) -> None:\n        self.reader = reader\n\n    def run(self):\n        frame = self.reader.read()\n        return frame[\"qty\"].sum()\n";
    let mut linter = Linter::new();
    linter.collect_suggestions = true;

    // act
    linter
        .check_file_internal(source, Path::new("m.py"))
        .unwrap();

    // assert: the library is read from the injected class, so a pandas selection is made
    assert_eq!(linter.suggestions.len(), 1, "{:?}", linter.withheld);
    assert!(linter.suggestions[0].diff.contains("[[\"qty\"]]"));
}

#[test]
fn test_should_read_the_library_from_a_method_of_an_instance_of_a_class_in_another_file() {
    // arrange
    let main = "from repo import Repo\n\n\ndef run():\n    repo = Repo()\n    frame = repo.fetch()\n    return frame[\"qty\"].sum()\n";

    // act
    let linter = suggest_with_repo_file(PANDAS_REPO, main);

    // assert
    assert_eq!(linter.suggestions.len(), 1, "{:?}", linter.withheld);
    assert!(linter.suggestions[0].diff.contains("[[\"qty\"]]"));
}

#[test]
fn test_should_read_the_library_from_a_parameter_annotated_with_a_class_in_another_file() {
    // arrange
    let main = "from repo import Repo\n\n\ndef run(repo: Repo):\n    frame = repo.fetch()\n    return frame[\"qty\"].sum()\n";

    // act
    let linter = suggest_with_repo_file(PANDAS_REPO, main);

    // assert
    assert_eq!(linter.suggestions.len(), 1, "{:?}", linter.withheld);
}

#[test]
fn test_should_read_the_library_from_a_self_method_inherited_from_a_class_in_another_file() {
    // arrange
    let main = "from repo import Repo\n\n\nclass Job(Repo):\n    def run(self):\n        frame = self.fetch()\n        return frame[\"qty\"].sum()\n";

    // act
    let linter = suggest_with_repo_file(PANDAS_REPO, main);

    // assert
    assert_eq!(linter.suggestions.len(), 1, "{:?}", linter.withheld);
}

#[test]
fn test_should_read_polars_from_a_method_of_a_class_in_another_file() {
    // arrange
    let repo = "import polars as pl\n\n\nclass Repo:\n    def fetch(self) -> pl.DataFrame:\n        return pl.read_csv(\"x.csv\")\n";
    let main = "from repo import Repo\n\n\ndef run(repo: Repo):\n    frame = repo.fetch()\n    return frame[\"qty\"].sum()\n";

    // act
    let linter = suggest_with_repo_file(repo, main);

    // assert
    assert_eq!(linter.suggestions.len(), 1, "{:?}", linter.withheld);
    assert!(
        linter.suggestions[0].diff.contains("select"),
        "{}",
        linter.suggestions[0].diff
    );
}

#[test]
fn test_should_not_read_the_library_from_an_imported_method_this_file_overrides() {
    // arrange: Job's own `fetch` says nothing about its library.
    let main = "from repo import Repo\n\n\nclass Job(Repo):\n    def fetch(self):\n        return load()\n\n    def run(self):\n        frame = self.fetch()\n        return frame[\"qty\"].sum()\n";

    // act
    let linter = suggest_with_repo_file(PANDAS_REPO, main);

    // assert
    assert!(linter.suggestions.is_empty());
    assert!(
        linter
            .withheld
            .iter()
            .any(|w| w.reason.contains("pandas or polars")),
        "{:?}",
        linter.withheld
    );
}

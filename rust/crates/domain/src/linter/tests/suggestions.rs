use super::*;

#[test]
fn test_should_suggest_a_selection_for_a_function_local_load() {
    // arrange
    let source = r#"
import pandas as pd


def load():
    df = pd.read_json("x.json")
    return df["a"] + df["b"]
"#;

    // act
    let linter = suggest_for(source);

    // assert
    assert!(linter.withheld.is_empty(), "{:?}", linter.withheld);
    assert_eq!(linter.suggestions.len(), 1);
    let s = &linter.suggestions[0];
    assert_eq!(s.tier, "safe");
    assert_eq!(s.columns, vec!["a", "b"]);
    assert_eq!(s.var, "df");
    assert_eq!(s.kind, "select");
    assert!(
        s.diff.contains("+    df = df[[\"a\", \"b\"]]"),
        "{}",
        s.diff
    );
}

#[test]
fn test_should_mark_a_module_level_suggestion_as_review() {
    let linter =
        suggest_for("import pandas as pd\ndf = pd.read_csv(\"x.csv\")\nprint(df[\"a\"])\n");
    assert_eq!(linter.suggestions.len(), 1);
    assert_eq!(linter.suggestions[0].tier, "review");
}

#[test]
fn test_should_withhold_when_the_frame_is_passed_to_a_helper() {
    let linter = suggest_for(
        "import pandas as pd\ndf = pd.read_csv(\"x.csv\")\nprint(df[\"a\"])\nclean(df)\n",
    );

    assert!(linter.suggestions.is_empty());
    assert_eq!(linter.withheld.len(), 1);
    assert_eq!(
            linter.withheld[0].reason,
            "can't see everything done with it: passed to `clean()`, a name this file never defines or imports (a builtin, or from a star import) (line 4)"
        );
}

#[test]
fn test_should_say_what_kind_of_callee_a_frame_was_passed_to() {
    let own_function =
        withheld_reason_for_call("", "def leaky(frame):\n    return frame\n", "leaky(df)");
    let imported = withheld_reason_for_call("from elsewhere import tidy", "", "tidy(df)");
    let module_function = withheld_reason_for_call("import numpy as np", "", "np.log(df)");
    let other_object = withheld_reason_for_call("", "", "other.merge(df)");
    let own_method = withheld_reason_for_call(
            "",
            "class Job:\n    def helper(self, frame):\n        return frame\n\n    def go(self):\n        df = pd.read_csv(\"y.csv\")\n        print(df[\"a\"])\n        self.helper(df)\n",
            "return None",
        );

    assert!(
        own_function
            .contains("passed to `leaky()`, a function that returns, stores or forwards the frame"),
        "{own_function}"
    );
    assert!(
        imported.contains("passed to `tidy()`, an imported function the checker has no summary of"),
        "{imported}"
    );
    assert!(
        module_function.contains("passed to `np.log()`, a function of an imported module"),
        "{module_function}"
    );
    assert!(
        other_object.contains("passed to `other.merge()`, a method of another object"),
        "{other_object}"
    );
    assert!(
        own_method.contains("a function that returns, stores or forwards the frame"),
        "{own_method}"
    );
}

#[test]
fn test_should_label_a_call_through_a_from_imported_name_as_such() {
    let reason = withheld_reason_for_call("from pkg import sub", "", "sub.fn(df)");

    assert!(
        reason.contains(
            "passed to `sub.fn()`, a function or method reached through an imported name"
        ),
        "{reason}"
    );
}

#[test]
fn test_should_not_label_a_call_on_an_attribute_of_an_object() {
    let reason = withheld_reason_for_call("", "", "other.client.send(df)");

    assert_eq!(
        reason,
        "can't see everything done with it: passed to a call (line 9)"
    );
}

#[test]
fn test_should_withhold_when_a_nested_function_mentions_the_frame() {
    // arrange: the closure reads `df` at call time, wherever it is defined.
    let source = r#"
import pandas as pd


def load():
    df = pd.read_csv("x.csv")
    print(df["a"])

    def helper():
        return df["z"]

    return helper
"#;

    // act
    let linter = suggest_for(source);

    // assert
    assert!(linter.suggestions.is_empty());
    assert!(
        linter.withheld[0].reason.contains(
            "also mentioned inside a nested function, class, lambda or generator expression"
        ),
        "{:?}",
        linter.withheld
    );
}

#[test]
fn test_should_withhold_when_a_function_defined_above_the_origin_reads_it() {
    let source = r#"
import pandas as pd


def report():
    return df["z"]


df = pd.read_csv("x.csv")
print(df["a"])
"#;
    let linter = suggest_for(source);
    assert!(linter.suggestions.is_empty());
    assert_eq!(linter.withheld.len(), 1);
    assert!(
        linter.withheld[0]
            .reason
            .contains("also mentioned inside a nested function"),
        "{:?}",
        linter.withheld
    );
}

#[test]
fn test_should_withhold_when_the_name_is_declared_global_in_the_scope() {
    let source = r#"
import pandas as pd


def load():
    global df
    df = pd.read_csv("x.csv")
    print(df["a"])
"#;
    let linter = suggest_for(source);
    assert!(linter.suggestions.is_empty());
    assert!(linter.withheld[0]
        .reason
        .contains("declared `global` or `nonlocal`"));
}

#[test]
fn test_should_account_for_a_class_attribute_origin_the_scan_never_reaches() {
    // arrange: an assignment directly in a class body is an attribute, not a local, so
    // it is never an origin here, but it must still be listed rather than missing.
    let source = r#"
import pandas as pd


class Job:
    df = pd.read_csv("x.csv")
    print(df["a"])
"#;
    let linter = suggest_for(source);

    assert!(linter.suggestions.is_empty());
    assert_eq!(linter.withheld.len(), 1);
    assert!(linter.withheld[0]
        .reason
        .contains("isn't a plain `name = ...` assignment"));
}

#[test]
fn test_should_not_suggest_for_a_site_that_is_already_typed() {
    let linter = suggest_for(
        "import pandas as pd\ndf = pd.read_csv(\"x.csv\", usecols=[\"a\"])\nprint(df[\"a\"])\n",
    );
    assert!(linter.suggestions.is_empty());
    assert!(linter.withheld.is_empty());
}

#[test]
fn test_should_read_the_library_from_a_function_imported_from_another_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    fs::write(
        root.join("loaders.py"),
        "import polars as pl\n\n\ndef load(path) -> pl.DataFrame:\n    return make(path)\n",
    )
    .unwrap();
    let main =
        "from loaders import load\n\n\ndef run():\n    df = load(\"x\")\n    return df[\"a\"]\n";
    fs::write(root.join("main.py"), main).unwrap();
    let index = build_index_internal(root);
    let mut linter = Linter::new();
    linter.collect_suggestions = true;
    let path = root.join("main.py");
    linter.load_cross_file_symbols(&index, main, &path, root);

    linter.check_file_internal(main, &path).unwrap();

    assert_eq!(linter.suggestions.len(), 1, "{:?}", linter.withheld);
    assert!(linter.suggestions[0]
        .diff
        .contains("+    df = df.select(\"a\")"));
}

#[test]
fn test_should_count_the_columns_a_same_file_helper_reads() {
    let source = r#"
import pandas as pd


def total(frame):
    return frame["a"].sum() + frame["b"].sum()


def run():
    df = pd.read_csv("x.csv")
    print(total(df))
    return df["c"]
"#;
    let linter = suggest_for(source);

    assert_eq!(linter.suggestions.len(), 1, "{:?}", linter.withheld);
    assert_eq!(linter.suggestions[0].columns, vec!["a", "b", "c"]);
    assert!(linter.suggestions[0].reason.contains("`total()`"));
}

#[test]
fn test_should_count_the_columns_an_imported_helper_reads() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    fs::write(
            root.join("helpers.py"),
            "def total(frame):\n    return frame[\"a\"].sum()\n\n\ndef leaky(frame):\n    return frame\n",
        )
        .unwrap();
    let main = "import pandas as pd\nfrom helpers import leaky, total\n\n\ndef run():\n    df = pd.read_csv(\"x.csv\")\n    print(total(df))\n    return df[\"b\"]\n\n\ndef run_leaky():\n    df = pd.read_csv(\"y.csv\")\n    return leaky(df)\n";
    fs::write(root.join("main.py"), main).unwrap();
    let index = build_index_internal(root);
    let mut linter = Linter::new();
    linter.collect_suggestions = true;
    let path = root.join("main.py");
    linter.load_cross_file_symbols(&index, main, &path, root);

    linter.check_file_internal(main, &path).unwrap();

    assert_eq!(linter.suggestions.len(), 1, "{:?}", linter.withheld);
    assert_eq!(linter.suggestions[0].columns, vec!["a", "b"]);
    assert_eq!(linter.withheld.len(), 1);
    assert!(linter.withheld[0].reason.contains("passed to `leaky()`"));
}

#[test]
fn test_should_count_the_columns_a_relatively_imported_helper_reads() {
    let main = "import pandas as pd\nfrom .helpers import total\n\n\ndef run():\n    df = pd.read_csv(\"x.csv\")\n    print(total(df))\n    return df[\"b\"]\n";

    let linter = suggest_in_project(
        &[
            ("app/__init__.py", ""),
            ("app/helpers.py", TOTAL_HELPER),
            ("app/main.py", main),
        ],
        "app/main.py",
    );

    assert_eq!(linter.suggestions.len(), 1, "{:?}", linter.withheld);
    assert_eq!(linter.suggestions[0].columns, vec!["a", "b"]);
}

#[test]
fn test_should_check_columns_against_a_relatively_imported_schema() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    fs::create_dir_all(root.join("app")).unwrap();
    fs::write(root.join("app/__init__.py"), "").unwrap();
    fs::write(
            root.join("app/schemas.py"),
            "from typedframes import BaseSchema, Column\n\n\nclass CustomerSchema(BaseSchema):\n    name = Column(type=str)\n",
        )
        .unwrap();
    let main = "from typing import Annotated\n\nimport pandas as pd\n\nfrom .schemas import CustomerSchema\n\n\ndef run(df: Annotated[pd.DataFrame, CustomerSchema]) -> None:\n    df[\"missing\"]\n";
    let main_path = root.join("app/main.py");
    fs::write(&main_path, main).unwrap();
    let index = build_index_internal(&root);
    let mut linter = Linter::new();
    linter.load_cross_file_symbols(&index, main, &main_path, &root);

    let errors = linter.check_file_internal(main, &main_path).unwrap();

    assert!(
        errors.iter().any(|e| e.code == "unknown-column"),
        "{errors:?}"
    );
}

#[test]
fn test_should_check_columns_against_a_lazily_imported_schema() {
    // arrange
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    fs::create_dir_all(root.join("app")).unwrap();
    fs::write(root.join("app/__init__.py"), "").unwrap();
    fs::write(
            root.join("app/schemas.py"),
            "from typedframes import BaseSchema, Column\n\n\nclass CustomerSchema(BaseSchema):\n    name = Column(type=str)\n",
        )
        .unwrap();
    let main = "lazy from typing import Annotated\n\nlazy import pandas as pd\n\nlazy from app.schemas import CustomerSchema\n\n\ndef run(df: Annotated[pd.DataFrame, CustomerSchema]) -> None:\n    df[\"missing\"]\n";
    let main_path = root.join("app/main.py");
    fs::write(&main_path, main).unwrap();
    let index = build_index_internal(&root);
    let mut linter = Linter::new();
    linter.load_cross_file_symbols(&index, main, &main_path, &root);

    // act
    let errors = linter.check_file_internal(main, &main_path).unwrap();

    // assert
    assert!(
        errors.iter().any(|e| e.code == "unknown-column"),
        "{errors:?}"
    );
}

#[test]
fn test_should_follow_a_relative_import_up_a_package() {
    let main = "import pandas as pd\nfrom ..core.helpers import total\n\n\ndef run():\n    df = pd.read_csv(\"x.csv\")\n    return total(df)\n";

    let linter = suggest_in_project(
        &[
            ("app/__init__.py", ""),
            ("app/core/__init__.py", ""),
            ("app/core/helpers.py", TOTAL_HELPER),
            ("app/sub/__init__.py", ""),
            ("app/sub/main.py", main),
        ],
        "app/sub/main.py",
    );

    assert_eq!(linter.suggestions.len(), 1, "{:?}", linter.withheld);
    assert_eq!(linter.suggestions[0].columns, vec!["a"]);
}

#[test]
fn test_should_withhold_when_a_relative_import_names_no_project_file() {
    let main = "import pandas as pd\nfrom .missing import total\n\n\ndef run():\n    df = pd.read_csv(\"x.csv\")\n    return total(df)\n";

    let linter = suggest_in_project(
        &[("app/__init__.py", ""), ("app/main.py", main)],
        "app/main.py",
    );

    assert!(linter.suggestions.is_empty());
    assert!(linter.withheld[0].reason.contains("passed to `total()`"));
}

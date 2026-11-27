use super::*;

#[test]
fn test_should_ignore_all_on_bare_ignore_comment() {
    // arrange — bare `# typedframes: ignore` suppresses all diagnostics on that line
    let source = r#"
from typedframes import BaseSchema, Column

class S(BaseSchema):
    user_id = Column(type=int)

import pandas as pd
df = pd.read_csv("data.csv", usecols=["user_id"])
print(df["revenue"])  # typedframes: ignore
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert — error on that line is suppressed
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
}

#[test]
fn test_should_ignore_specific_code() {
    // arrange — `# typedframes: ignore[unknown-column]` suppresses only that code
    let source = r#"
from typedframes import BaseSchema, Column

class S(BaseSchema):
    user_id = Column(type=int)

import pandas as pd
df = pd.read_csv("data.csv", usecols=["user_id"])
print(df["revenue"])  # typedframes: ignore[unknown-column]
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert — unknown-column on that line is suppressed
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
}

#[test]
fn test_should_not_ignore_mismatched_code() {
    // arrange — `# typedframes: ignore[dropped-unknown-column]` does not suppress unknown-column
    let source = r#"
from typedframes import BaseSchema, Column

class S(BaseSchema):
    user_id = Column(type=int)

import pandas as pd
df = pd.read_csv("data.csv", usecols=["user_id"])
print(df["revenue"])  # typedframes: ignore[dropped-unknown-column]
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert — wrong code in brackets, so error is NOT suppressed
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
}

#[test]
fn test_should_ignore_comma_separated_codes() {
    // arrange — `# typedframes: ignore[unknown-column, dropped-unknown-column]`
    let source = r#"
from typedframes import BaseSchema, Column

class S(BaseSchema):
    user_id = Column(type=int)

import pandas as pd
df = pd.read_csv("data.csv", usecols=["user_id"])
print(df["revenue"])  # typedframes: ignore[unknown-column, dropped-unknown-column]
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert — unknown-column is in the comma-separated list, so suppressed
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
}

#[test]
fn test_should_detect_missing_column_through_plain_module_import_attribute_call() {
    // arrange: same shape as test_should_detect_missing_column_at_direct_call_site,
    // but pipeline.py calls `steps.postproc(df)` via a plain `import steps` rather
    // than `from steps import postproc` — the blind spot this test guards against.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    fs::write(
        root.join("loaders.py"),
        r#"
import pandas as pd

def load(path: str) -> pd.DataFrame:
    df = pd.read_csv(path, usecols=["a", "b"])
    return df
"#,
    )
    .unwrap();
    fs::write(
        root.join("steps.py"),
        r#"
import pandas as pd

def postproc(df: pd.DataFrame) -> pd.DataFrame:
    y = df["c"]
    return df
"#,
    )
    .unwrap();
    let pipeline_source = r#"
import steps
from loaders import load

def process(path: str) -> None:
    df = load(path)
    result = steps.postproc(df)
    print(result)
"#;
    fs::write(root.join("pipeline.py"), pipeline_source).unwrap();

    // act
    let index = build_index_internal(root);
    let mut linter = Linter::new();
    let pipeline_path = root.join("pipeline.py");
    linter.load_cross_file_symbols(&index, pipeline_source, &pipeline_path, root);
    let errors = linter
        .check_file_internal(pipeline_source, &pipeline_path)
        .unwrap();

    // assert
    assert_eq!(errors.len(), 1, "expected one error, got: {errors:?}");
    assert_eq!(errors[0].code, "missing-column");
    assert!(errors[0].message.contains("missing column(s) {c}"));
    assert!(errors[0].message.contains("passed to postproc"));
}

#[test]
fn test_should_detect_missing_column_through_wildcard_import() {
    // arrange: pipeline.py brings postproc into scope via `from steps import *`
    // rather than naming it explicitly.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    fs::write(
        root.join("loaders.py"),
        r#"
import pandas as pd

def load(path: str) -> pd.DataFrame:
    df = pd.read_csv(path, usecols=["a", "b"])
    return df
"#,
    )
    .unwrap();
    fs::write(
        root.join("steps.py"),
        r#"
import pandas as pd

def postproc(df: pd.DataFrame) -> pd.DataFrame:
    y = df["c"]
    return df
"#,
    )
    .unwrap();
    let pipeline_source = r#"
from steps import *
from loaders import load

def process(path: str) -> None:
    df = load(path)
    result = postproc(df)
    print(result)
"#;
    fs::write(root.join("pipeline.py"), pipeline_source).unwrap();

    // act
    let index = build_index_internal(root);
    let mut linter = Linter::new();
    let pipeline_path = root.join("pipeline.py");
    linter.load_cross_file_symbols(&index, pipeline_source, &pipeline_path, root);
    let errors = linter
        .check_file_internal(pipeline_source, &pipeline_path)
        .unwrap();

    // assert
    assert_eq!(errors.len(), 1, "expected one error, got: {errors:?}");
    assert_eq!(errors[0].code, "missing-column");
    assert!(errors[0].message.contains("missing column(s) {c}"));
    assert!(errors[0].message.contains("passed to postproc"));
}

#[test]
fn test_should_read_the_line_from_an_inferred_name_with_a_collision_suffix() {
    assert_eq!(
        inferred_schema_line("__inferred_df_0000abcd_at_12"),
        Some(12)
    );
    assert_eq!(
        inferred_schema_line("__inferred_df_0000abcd_at_12_2"),
        Some(12)
    );
    assert_eq!(
        inferred_schema_line("__inferred_a_at_b_0000abcd_at_7"),
        Some(7)
    );
    assert_eq!(inferred_schema_line("UserSchema"), None);
}

#[test]
fn test_should_apply_explicit_merge_suffixes_to_the_result_columns() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\nusers = pd.DataFrame({\"user_id\": [1], \"segment\": [\"a\"], \"ctr\": [0.1]})\nseg = pd.DataFrame({\"segment\": [\"a\"], \"ctr\": [0.2]})\ndf = users.merge(seg, on=\"segment\", suffixes=(\"\", \"_seg\"))\nprint(df[\"ctr\"] - df[\"ctr_seg\"])\ndf2 = users.merge(seg, on=\"segment\")\nprint(df2[\"ctr_x\"], df2[\"ctr_y\"])\n",
        );

    // assert
    assert!(errors.is_empty(), "{errors:#?}");
}

#[test]
fn test_should_report_a_bare_overlapping_name_after_a_default_merge() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\nusers = pd.DataFrame({\"user_id\": [1], \"segment\": [\"a\"], \"ctr\": [0.1]})\nseg = pd.DataFrame({\"segment\": [\"a\"], \"ctr\": [0.2]})\ndf = users.merge(seg, on=\"segment\", how=\"left\")\nprint(df[\"ctr\"].mean())\nprint(df[\"segment\"])\n",
        );

    // assert: only the collided name, never the join key
    assert_eq!(codes_of(&errors), [("unknown-column", 6)], "{errors:#?}");
    assert!(errors[0].message.contains("'ctr'"));
}

#[test]
fn test_should_merge_a_literal_selection_receiver_with_a_derived_operand() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\ndef drift(feats: pd.DataFrame) -> pd.DataFrame:\n    last = feats.groupby(\"item_id\").tail(1)[[\"item_id\", \"i_ctr\"]]\n    d = feats[[\"item_id\", \"i_ctr\"]].merge(last, on=\"item_id\", how=\"left\")\n    d[\"delta\"] = d[\"i_ctr_x\"] - d[\"i_ctr_y\"]\n    d[\"bad\"] = d[\"i_ctr_last\"]\n    return d\n",
        );

    // assert
    assert_eq!(codes_of(&errors), [("unknown-column", 8)], "{errors:#?}");
}

#[test]
fn test_should_not_track_a_concat_with_an_untracked_operand_as_unresolved() {
    // arrange / act
    let (linter, errors) = lint_source(
            "import pandas as pd\n\nimp = pd.DataFrame({\"x\": [1], \"click\": [0]})\nlate = load_late()\nimp = pd.concat([imp, late])\nprint(imp.click.mean())\nprint(imp[\"click\"])\n",
        );

    // assert: no claim about an empty column set after tracking was given up
    assert!(errors.is_empty(), "{errors:#?}");
    assert!(linter.variables.get("imp").is_none());
}

#[test]
fn test_should_union_every_operand_of_a_concat() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\na = pd.DataFrame({\"x\": [1]})\nb = pd.DataFrame({\"y\": [1]})\nc = pd.DataFrame({\"z\": [1]})\nall_ = pd.concat([a, b, c])\nprint(all_[\"z\"], all_[\"w\"])\n",
        );

    // assert: `z` comes from the third operand
    assert_eq!(codes_of(&errors), [("unknown-column", 7)], "{errors:#?}");
    assert!(errors[0].message.contains("'w'"));
}

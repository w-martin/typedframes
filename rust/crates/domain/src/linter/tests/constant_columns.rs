use super::*;

#[test]
fn test_should_resolve_a_column_list_built_from_other_constants() {
    let source = r#"
import pandas as pd

BASE = ["id", "amount"]
COLS = BASE + [f"m{i}" for i in range(3)]
df = pd.read_csv("f.csv", usecols=COLS)
print(df["m2"])
print(df["nope"])
"#;
    let (linter, errors) = lint(source);

    assert_eq!(linter.dataframes_typed, 1);
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("{id, amount, m0, m1, m2}"));
}

#[test]
fn test_should_resolve_a_column_list_unpacked_in_a_comprehension() {
    // arrange
    let source = r#"
import pandas as pd

df = pd.read_csv("f.csv", usecols=[*[f"{p}_x", f"{p}_y"] for p in ["a", "b"]])
print(df["b_y"])
print(df["nope"])
"#;

    // act
    let (linter, errors) = lint(source);

    // assert
    assert_eq!(linter.dataframes_typed, 1);
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("{a_x, a_y, b_x, b_y}"));
}

#[test]
fn test_should_rename_through_a_mapping_merged_in_a_comprehension() {
    // arrange
    let source = r#"
import pandas as pd

df = pd.read_csv("f.csv", usecols=["user_id", "email"])
out = df.rename(columns={**m for m in [{"email": "mail"}, {"user_id": "uid"}]})
print(out["uid"], out["mail"])
print(out["email"])
"#;

    // act
    let (_, errors) = lint(source);

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'email'"), "{errors:?}");
    assert!(errors[0].message.contains("{uid, mail}"), "{errors:?}");
}

#[test]
fn test_should_rename_through_a_frozendict_mapping() {
    // arrange
    let source = r#"
import pandas as pd

df = pd.read_csv("f.csv", usecols=["user_id", "email"])
out = df.rename(columns=frozendict(email="mail"))
out2 = out.rename(columns=frozendict({"user_id": "uid"}))
print(out2["uid"], out2["mail"])
print(out2["email"])
"#;

    // act
    let (_, errors) = lint(source);

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'email'"), "{errors:?}");
    assert!(errors[0].message.contains("{uid, mail}"), "{errors:?}");
}

#[test]
fn test_should_rename_through_the_frozendict_backport_before_python_3_15() {
    // arrange
    let source = r#"
import pandas as pd
from frozendict import frozendict

df = pd.read_csv("f.csv", usecols=["user_id", "email"])
out = df.rename(columns=frozendict(email="mail"))
print(out["mail"])
print(out["email"])
"#;

    // act
    let (_, errors) = lint(source);

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("{user_id, mail}"), "{errors:?}");
}

#[test]
fn test_should_resolve_lazy_imports_like_eager_ones() {
    // arrange
    let body = r#"
class UserSchema(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)


def f(df: Annotated[pd.DataFrame, UserSchema]) -> None:
    print(df["emai"])


def g() -> None:
    df = pd.read_csv("u.csv", usecols=["user_id", "email"])
    print(df["nope"])
"#;
    let eager = format!("from typing import Annotated\nimport pandas as pd\nfrom typedframes import BaseSchema, Column\n{body}");
    let lazy = format!("lazy from typing import Annotated\nlazy import pandas as pd\nlazy from typedframes import BaseSchema, Column\n{body}");

    // act
    let (_, eager_errors) = lint(&eager);
    let (_, lazy_errors) = lint(&lazy);

    // assert
    let codes = |errors: &[LintError]| -> Vec<(usize, String)> {
        errors
            .iter()
            .map(|e| (e.line, e.code.to_string()))
            .collect()
    };
    assert_eq!(eager_errors.len(), 2, "{eager_errors:?}");
    assert_eq!(codes(&lazy_errors), codes(&eager_errors));
}

#[test]
fn test_should_resolve_columns_returned_by_a_helper_function() {
    let source = r#"
import pandas as pd


def feature_cols():
    return ["a", "b"]


def prefixed(prefix):
    return [f"{prefix}_x", f"{prefix}_y"]


df = pd.read_csv("f.csv", usecols=feature_cols())
other = pd.read_csv("g.csv", usecols=prefixed("m"))
print(df["c"])
print(other["m_x"])
print(other["m_z"])
"#;
    let (linter, errors) = lint(source);

    assert_eq!(linter.dataframes_typed, 2);
    assert_eq!(errors.len(), 2, "errors: {errors:?}");
    assert!(errors[0].message.contains("'c'") && errors[0].message.contains("{a, b}"));
    assert!(errors[1].message.contains("'m_z'") && errors[1].message.contains("{m_x, m_y}"));
}

#[test]
fn test_should_resolve_columns_read_from_a_class_attribute() {
    let source = r#"
import polars as pl


class Cols:
    ALL = ["a", "b", "c"]
    KEEP = ["a", "b"]


df = pl.read_parquet("x.parquet", columns=Cols.ALL)
out = df.select(Cols.KEEP)
print(out["a"])
print(out["c"])
"#;
    let (_, errors) = lint(source);
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'c'") && errors[0].message.contains("{a, b}"));
}

#[test]
fn test_should_not_resolve_a_helper_that_reads_runtime_state() {
    let source = r#"
import os
import pandas as pd


def cols():
    return os.environ["COLS"].split(",")


df = pd.read_csv("f.csv", usecols=cols())
"#;
    assert_eq!(lint(source).0.dataframes_typed, 0);
}

#[test]
fn test_should_resolve_a_helper_that_uses_a_module_constant() {
    let source = r#"
import pandas as pd

BASE = ["id"]


def cols(extra):
    return BASE + [extra]


df = pd.read_csv("f.csv", usecols=cols("amount"))
print(df["amount"])
print(df["zz"])
"#;
    let (linter, errors) = lint(source);
    assert_eq!(linter.dataframes_typed, 1);
    assert!(
        errors[0].message.contains("{id, amount}"),
        "errors: {errors:?}"
    );
}

#[test]
fn test_should_resolve_each_functions_own_local_column_list() {
    let source = r#"
import pandas as pd


def a():
    cols = ["x"]
    df = pd.read_csv("a.csv", usecols=cols)
    return df


def b():
    cols = ["y"]
    df = pd.read_csv("b.csv", usecols=cols)
    return df
"#;
    let (linter, _) = lint(source);
    assert_eq!(linter.dataframes_typed, 2);
}

#[test]
fn test_should_resolve_a_module_constant_used_inside_a_function() {
    let source = r#"
import pandas as pd

COLS = ["a", "b"]


def load():
    df = pd.read_parquet("x.parquet", columns=COLS)
    return df
"#;
    assert_eq!(lint(source).0.dataframes_typed, 1);
}

#[test]
fn test_should_resolve_a_polars_select_through_a_list_variable() {
    let source = r#"
import polars as pl

KEEP = ["a", "b"]
df = pl.read_parquet("x.parquet", columns=["a", "b", "c"])
out = df.select(KEEP)
print(out["a"])
print(out["c"])
"#;
    let (_, errors) = lint(source);
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'c'") && errors[0].message.contains("{a, b}"));
}

#[test]
fn test_should_narrow_through_a_list_variable_subscript() {
    let source = r#"
import pandas as pd

KEEP = ["a", "b"]
df = pd.read_csv("f.csv", usecols=["a", "b", "c"])
out = df[KEEP]
print(out["c"])
"#;
    let (_, errors) = lint(source);
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'c'") && errors[0].message.contains("{a, b}"));
}

#[test]
fn test_should_validate_a_column_name_held_in_a_variable() {
    let source = r#"
import pandas as pd

df = pd.read_csv("f.csv", usecols=["a", "b"])
GOOD = "a"
BAD = "zzz"
print(df[GOOD])
print(df[BAD])
"#;
    let (_, errors) = lint(source);
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'zzz'"));
}

#[test]
fn test_should_track_a_rename_through_a_mapping_variable() {
    let source = r#"
import pandas as pd

MAPPING = {"a": "x"}
df = pd.read_csv("f.csv", usecols=["a", "b"])
out = df.rename(columns=MAPPING)
print(out["x"])
print(out["a"])
"#;
    let (_, errors) = lint(source);
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'a'"));
}

#[test]
fn test_should_resolve_a_conditional_constant() {
    let source = r#"
import pandas as pd

DEBUG = False
COLS = ["a"] if DEBUG else ["b"]
df = pd.read_csv("f.csv", usecols=COLS)
print(df["a"])
"#;
    let (linter, errors) = lint(source);
    assert_eq!(linter.dataframes_typed, 1);
    assert!(errors[0].message.contains("{b}"), "errors: {errors:?}");
}

#[test]
fn test_should_not_resolve_what_depends_on_runtime_state() {
    for rhs in [
        "open(\"cols.txt\").read().split()",
        "os.environ[\"COLS\"].split(\",\")",
        "sys.argv[1:]",
        "get_cols()",
        "list(other.columns)",
    ] {
        let source = format!(
            "import pandas as pd\n\nCOLS = {rhs}\ndf = pd.read_csv(\"f.csv\", usecols=COLS)\n"
        );
        assert_eq!(lint(&source).0.dataframes_typed, 0, "{rhs}");
    }
}

#[test]
fn test_should_stop_resolving_a_list_that_is_changed_or_passed_on() {
    for hazard in [
        "COLS.append(\"z\")",
        "helper(COLS)",
        "alias = COLS",
        "COLS[0] = \"z\"",
        "COLS += [\"z\"]",
    ] {
        let source = format!("import pandas as pd\n\nCOLS = [\"a\"]\n{hazard}\ndf = pd.read_csv(\"f.csv\", usecols=COLS)\n");
        assert_eq!(lint(&source).0.dataframes_typed, 0, "{hazard}");
    }
}

#[test]
fn test_should_resolve_a_list_that_is_only_read_elsewhere() {
    let source = r#"
import pandas as pd

COLS = ["a", "b"]
print(len(COLS), "a" in COLS)
for c in COLS:
    print(c)
df = pd.read_csv("f.csv", usecols=COLS)
"#;
    assert_eq!(lint(source).0.dataframes_typed, 1);
}

#[test]
fn test_should_leave_everything_else_in_the_file_untouched_by_folding() {
    // a file that folds in one place must not change how an unrelated, untracked
    // frame is handled
    let source = r#"
import pandas as pd

COLS = ["a"]
typed = pd.read_csv("f.csv", usecols=COLS)
untyped = pd.read_csv("g.csv")
print(untyped["whatever"])
"#;
    let (linter, errors) = lint(source);
    assert_eq!(linter.dataframes_total, 2);
    assert_eq!(linter.dataframes_typed, 1);
    assert!(
        errors.iter().all(|e| e.code != CODE_UNKNOWN_COLUMN),
        "errors: {errors:?}"
    );
}

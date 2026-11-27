use super::*;

#[test]
fn test_traces_sql_through_a_single_assigned_variable() {
    // A module-level (or function-local — the checker doesn't distinguish, see
    // StringBindingCollector) constant assigned exactly once should resolve just
    // like an inline literal.
    let source = r#"
import pandas as pd

QUERY = "SELECT order_id, amount FROM orders"
df = pd.read_sql(QUERY, conn)
print(df["order_id"])
print(df["missing"])
"#;
    let mut linter = Linter::new();
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("missing"));
}

#[test]
fn test_reassigned_sql_variable_is_not_traced() {
    // A second binding of QUERY poisons it — the checker must not guess which
    // assignment was actually in effect at the read_sql call, so the load falls
    // through to the untracked-dataframe hint instead of inferring a (possibly
    // wrong) column set.
    let source = r#"
import pandas as pd

QUERY = "SELECT order_id FROM orders"
QUERY = "SELECT customer_id FROM customers"
df = pd.read_sql(QUERY, conn)
"#;
    let mut linter = Linter::new();
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNTRACKED_DATAFRAME);
}

#[test]
fn test_traces_sql_from_a_file_under_the_project_root() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(
        root.join("orders.sql"),
        "SELECT order_id, amount FROM orders",
    )
    .unwrap();

    let source = r#"
import pandas as pd
from pathlib import Path

sql = Path("orders.sql").read_text()
df = pd.read_sql(sql, conn)
print(df["order_id"])
print(df["missing"])
"#;
    let mut linter = Linter::new();
    linter.fs = Arc::new(OsFiles).into();
    linter.with_context(root.to_path_buf(), &LinterConfig::EMPTY);
    let errors = linter
        .check_file_internal(source, &root.join("pipeline.py"))
        .unwrap();

    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("missing"));
}

#[test]
fn test_file_read_refuses_path_escaping_project_root() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let outside = temp.path().parent().unwrap();
    fs::write(outside.join("secret.sql"), "SELECT * FROM secrets").unwrap();

    let source = r#"
import pandas as pd
from pathlib import Path

sql = Path("../secret.sql").read_text()
df = pd.read_sql(sql, conn)
"#;
    let mut linter = Linter::new();
    linter.fs = Arc::new(OsFiles).into();
    linter.with_context(root.to_path_buf(), &LinterConfig::EMPTY);
    let errors = linter
        .check_file_internal(source, &root.join("pipeline.py"))
        .unwrap();

    // Unresolved -> falls through to untracked-dataframe, never a fabricated
    // column set from a file outside the project.
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNTRACKED_DATAFRAME);
}

#[test]
fn test_file_read_refuses_non_sql_extension() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(root.join("orders.txt"), "SELECT order_id FROM orders").unwrap();

    let source = r#"
import pandas as pd
from pathlib import Path

sql = Path("orders.txt").read_text()
df = pd.read_sql(sql, conn)
"#;
    let mut linter = Linter::new();
    linter.fs = Arc::new(OsFiles).into();
    linter.with_context(root.to_path_buf(), &LinterConfig::EMPTY);
    let errors = linter
        .check_file_internal(source, &root.join("pipeline.py"))
        .unwrap();

    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNTRACKED_DATAFRAME);
}

#[test]
fn test_traces_sql_from_file_via_path_file_parent() {
    // Path(__file__).parent / "orders.sql" resolves to an ABSOLUTE path (since the
    // file being checked is passed in as an absolute path by every real caller) —
    // regression test for a bug where read_sql_file unconditionally refused
    // absolute paths and could never actually accept this legitimate idiom.
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(
        root.join("orders.sql"),
        "SELECT order_id, amount FROM orders",
    )
    .unwrap();

    let source = r#"
import pandas as pd
from pathlib import Path

sql = (Path(__file__).parent / "orders.sql").read_text()
df = pd.read_sql(sql, conn)
print(df["order_id"])
print(df["missing"])
"#;
    let mut linter = Linter::new();
    linter.fs = Arc::new(OsFiles).into();
    linter.with_context(root.to_path_buf(), &LinterConfig::EMPTY);
    let errors = linter
        .check_file_internal(source, &root.join("pipeline.py"))
        .unwrap();

    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("missing"));
}

#[test]
fn test_file_read_refuses_absolute_path_outside_root() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let outside = temp.path().parent().unwrap();
    let secret_path = outside.join("secret.sql");
    fs::write(&secret_path, "SELECT * FROM secrets").unwrap();

    let source = format!(
        r#"
import pandas as pd
from pathlib import Path

sql = Path("{}").read_text()
df = pd.read_sql(sql, conn)
"#,
        secret_path.display()
    );
    let mut linter = Linter::new();
    linter.fs = Arc::new(OsFiles).into();
    linter.with_context(root.to_path_buf(), &LinterConfig::EMPTY);
    let errors = linter
        .check_file_internal(&source, &root.join("pipeline.py"))
        .unwrap();

    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNTRACKED_DATAFRAME);
}

#[test]
fn test_fstring_sql_with_a_runtime_value_is_silently_untracked_not_a_new_diagnostic() {
    // There's no taint analysis to tell a safe interpolation from an injection, so a
    // runtime-valued f-string query gets only the usual untracked-dataframe hint.
    let source = r#"
import pandas as pd

cols = input()
df = pd.read_sql(f"SELECT {cols} FROM orders", conn)
"#;
    let mut linter = Linter::new();
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNTRACKED_DATAFRAME);
}

#[test]
fn test_fstring_sql_with_a_constant_value_is_resolved() {
    // A constant interpolation makes the query text known, so its columns can be read.
    let source = r#"
import pandas as pd

cols = "order_id, amount"
table = "orders"
df = pd.read_sql(f"SELECT {cols} FROM {table}", conn)
print(df["order_id"])
print(df["nope"])
"#;
    let (typed, errors) = typed_count(source);

    assert_eq!(typed, 1);
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'nope'"));
    assert!(errors[0].message.contains("{order_id, amount}"));
}

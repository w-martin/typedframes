use super::*;

#[test]
fn test_should_report_an_external_bare_dataframe_return_as_unverifiable() {
    // arrange: a py.typed internal library method annotated `-> pd.DataFrame` with
    // no Schema, called through self.<attr> and subscripted with a typo.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let pkg_dir = root
        .join(".venv")
        .join("lib")
        .join("python3.12")
        .join("site-packages")
        .join("internal_client_pkg");
    fs::create_dir_all(&pkg_dir).unwrap();
    fs::write(pkg_dir.join("py.typed"), "").unwrap();
    fs::write(
        pkg_dir.join("__init__.py"),
        r#"
import pandas as pd


class Client:
    def get_data(self, query: str) -> pd.DataFrame:
        return pd.read_sql(query, None)
"#,
    )
    .unwrap();

    let pipeline_source = r#"
from internal_client_pkg import Client


class Pipeline:
    def __init__(self):
        self._client = Client()

    def run(self):
        df = self._client.get_data("SELECT * FROM orders")
        print(df["typo_column"])
"#;
    fs::write(root.join("pipeline.py"), pipeline_source).unwrap();
    fs::write(root.join("pyproject.toml"), "[tool.typedframes]\n").unwrap();

    // act
    let index = build_index_internal(root);
    let mut linter = Linter::new();
    linter.fs = Arc::new(OsFiles).into();
    linter.with_context(root.to_path_buf(), &LinterConfig::EMPTY);
    let pipeline_path = root.join("pipeline.py");
    linter.load_cross_file_symbols(&index, pipeline_source, &pipeline_path, root);
    let errors = linter
        .check_file_internal(pipeline_source, &pipeline_path)
        .unwrap();

    // assert: the access raises unverifiable-column (not unknown-column -- this
    // checker has no basis to claim the column definitely doesn't exist), and
    // the origin does NOT count as typed.
    assert!(
        !errors.iter().any(|e| e.code == CODE_UNKNOWN_COLUMN),
        "expected no unknown-column diagnostic, got: {errors:?}"
    );
    assert!(
        errors.iter().any(|e| e.code == CODE_UNVERIFIABLE_COLUMN),
        "expected an unverifiable-column diagnostic, got: {errors:?}"
    );
    assert_eq!(linter.dataframes_total, 1);
    assert_eq!(linter.dataframes_typed, 0);
    assert_eq!(linter.untyped_sites.len(), 1);
    assert_eq!(linter.untyped_sites[0].var, "df");
    assert_eq!(linter.untyped_sites[0].line, 10);
}

#[test]
fn test_should_not_trace_a_non_py_typed_external_package_via_self_attr_without_usage() {
    // arrange: the exact same fixture as the py.typed test above, MINUS the
    // py.typed marker, and with the result still not used like a DataFrame --
    // must NOT be traced. This is the control proving py.typed is what made the
    // difference above, not some other unrelated resolution path.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let site_packages = root
        .join(".venv")
        .join("lib")
        .join("python3.12")
        .join("site-packages");
    let pkg_dir = site_packages.join("internal_repo_pkg");
    fs::create_dir_all(&pkg_dir).unwrap();
    fs::write(
        pkg_dir.join("__init__.py"),
        r#"
import pandas as pd


class DataRepository:
    def get(self, query: str) -> pd.DataFrame:
        return pd.read_sql(query, None)
"#,
    )
    .unwrap();

    let pipeline_source = r#"
from internal_repo_pkg import DataRepository


class Pipeline:
    def __init__(self):
        self._data_repository = DataRepository()

    def run(self):
        df = self._data_repository.get("SELECT * FROM training")
        return df
"#;
    fs::write(root.join("pipeline.py"), pipeline_source).unwrap();
    fs::write(root.join("pyproject.toml"), "[tool.typedframes]\n").unwrap();

    // act
    let index = build_index_internal(root);
    let mut linter = Linter::new();
    linter.fs = Arc::new(OsFiles).into();
    linter.with_context(root.to_path_buf(), &LinterConfig::EMPTY);
    let pipeline_path = root.join("pipeline.py");
    linter.load_cross_file_symbols(&index, pipeline_source, &pipeline_path, root);
    linter
        .check_file_internal(pipeline_source, &pipeline_path)
        .unwrap();

    // assert
    assert_eq!(linter.dataframes_total, 0);
    assert_eq!(linter.dataframes_typed, 0);
}

#[test]
fn test_should_respect_excluded_external_packages_opt_out() {
    // arrange: same auto-trace-eligible fixture, but the package is named in
    // `excluded_external_packages` -- must stay untraced even though usage looks
    // exactly as DataFrame-shaped as the default-on test above.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let site_packages = root
        .join(".venv")
        .join("lib")
        .join("python3.12")
        .join("site-packages");
    let pkg_dir = site_packages.join("internal_snowflake_pkg");
    fs::create_dir_all(&pkg_dir).unwrap();
    fs::write(
        pkg_dir.join("__init__.py"),
        r#"
import pandas as pd

def load_orders(query: str) -> pd.DataFrame:
    conn = connect()
    cursor = conn.cursor()
    cursor.execute("SELECT order_id, amount FROM orders")
    df = cursor.fetch_pandas_all()
    return df
"#,
    )
    .unwrap();

    let pipeline_source = r#"
from internal_snowflake_pkg import load_orders

def process(query: str) -> None:
    orders = load_orders(query)
    print(orders["order_id"])
"#;
    fs::write(root.join("pipeline.py"), pipeline_source).unwrap();
    fs::write(
        root.join("pyproject.toml"),
        "[tool.typedframes]\nexcluded_external_packages = [\"internal_snowflake_pkg\"]\n",
    )
    .unwrap();

    let config = LinterConfig {
        enabled: None,
        warnings: None,
        sql_dialect: None,
        trace_external_packages: None,
        excluded_external_packages: Some(vec!["internal_snowflake_pkg".to_string()]),
        exclude: None,
    };

    // act
    let index = build_index_internal(root);
    let mut linter = Linter::new();
    linter.fs = Arc::new(OsFiles).into();
    linter.with_context(root.to_path_buf(), &config);
    let pipeline_path = root.join("pipeline.py");
    linter.load_cross_file_symbols(&index, pipeline_source, &pipeline_path, root);
    let errors = linter
        .check_file_internal(pipeline_source, &pipeline_path)
        .unwrap();

    // assert: load_orders' return type stays unknown -- opted out despite looking
    // exactly like an auto-trace candidate
    assert_eq!(
        errors.len(),
        0,
        "excluded_external_packages should have kept this untraced: {errors:?}"
    );
}

#[test]
fn test_should_resolve_sql_loaded_from_file_inside_a_traced_external_package() {
    // arrange: the exact shape reported in production use -- an internal,
    // pip-installed package that executes SQL loaded from a `.sql` file sitting
    // next to its own source, not an inline string literal. Same
    // trace_external_packages allowlist mechanism as
    // test_should_trace_an_allowlisted_package_installed_in_venv_site_packages,
    // but load_orders resolves its query via
    // `Path(__file__).parent / "orders.sql"` -- the path must anchor to the
    // EXTERNAL package's own directory, not the caller's.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let site_packages = root
        .join(".venv")
        .join("lib")
        .join("python3.12")
        .join("site-packages");
    let pkg_dir = site_packages.join("internal_snowflake_pkg");
    fs::create_dir_all(&pkg_dir).unwrap();
    fs::write(
        pkg_dir.join("orders.sql"),
        "SELECT order_id, amount FROM orders",
    )
    .unwrap();
    fs::write(
        pkg_dir.join("__init__.py"),
        r#"
from pathlib import Path
import pandas as pd

def load_orders(conn) -> pd.DataFrame:
    sql = (Path(__file__).parent / "orders.sql").read_text()
    df = pd.read_sql(sql, conn)
    return df
"#,
    )
    .unwrap();

    let pipeline_source = r#"
from internal_snowflake_pkg import load_orders

def process(conn) -> None:
    orders = load_orders(conn)
    print(orders["order_id"])
    print(orders["customer_id"])
"#;
    fs::write(root.join("pipeline.py"), pipeline_source).unwrap();
    fs::write(
        root.join("pyproject.toml"),
        "[tool.typedframes]\ntrace_external_packages = [\"internal_snowflake_pkg\"]\n",
    )
    .unwrap();

    let config = LinterConfig {
        enabled: None,
        warnings: None,
        sql_dialect: None,
        trace_external_packages: Some(vec!["internal_snowflake_pkg".to_string()]),
        excluded_external_packages: None,
        exclude: None,
    };

    // act
    let index = build_index_internal(root);
    let mut linter = Linter::new();
    linter.fs = Arc::new(OsFiles).into();
    linter.with_context(root.to_path_buf(), &config);
    let pipeline_path = root.join("pipeline.py");
    linter.load_cross_file_symbols(&index, pipeline_source, &pipeline_path, root);
    let errors = linter
        .check_file_internal(pipeline_source, &pipeline_path)
        .unwrap();

    // assert: load_orders' return schema should resolve to {order_id, amount} via
    // the .sql file, so 'customer_id' -- not in that set -- should be flagged.
    assert_eq!(errors.len(), 1, "expected one error, got: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
    assert!(errors[0].message.contains("customer_id"));
}

#[test]
fn test_should_resolve_param_schema_defined_in_a_third_file_as_call_site_contract() {
    // arrange: schemas.py defines ReportSchema; reports.py's build_report() takes
    // a parameter annotated Annotated[pd.DataFrame, ReportSchema] but does not
    // itself define ReportSchema; pipeline.py calls build_report with a frame
    // that's missing columns ReportSchema declares but build_report's own body
    // never literally subscripts (so the heuristic body-scan fallback alone
    // would under-report the contract). The schema-authoritative override must
    // resolve project-wide, not just when the schema happens to be local.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    fs::write(
        root.join("schemas.py"),
        r#"
from typedframes import BaseSchema, Column

class ReportSchema(BaseSchema):
    a = Column(type=int)
    b = Column(type=int)
    c = Column(type=int)
"#,
    )
    .unwrap();
    fs::write(
        root.join("reports.py"),
        r#"
from typing import Annotated
import pandas as pd
from schemas import ReportSchema

def build_report(df: Annotated[pd.DataFrame, ReportSchema]) -> None:
    print(df["a"])
"#,
    )
    .unwrap();
    let pipeline_source = r#"
import pandas as pd
from reports import build_report

def process(path: str) -> None:
    df = pd.read_csv(path, usecols=["a"])
    build_report(df)
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

    // assert: 'b' and 'c' are declared on ReportSchema but never subscripted in
    // build_report's own body — only the authoritative schema override catches this.
    assert_eq!(errors.len(), 1, "expected one error, got: {errors:?}");
    assert_eq!(errors[0].code, "missing-column");
    assert!(errors[0].message.contains("missing column(s) {b, c}"));
    assert!(errors[0].message.contains("required: {a, b, c}"));
}

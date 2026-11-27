use super::*;

#[test]
fn test_should_propagate_case_folded_schema_from_an_unannotated_cross_file_helper() {
    // arrange: an internal-package-style helper in loaders.py queries Snowflake
    // (columns come back upper-cased under sql_dialect="snowflake"), then
    // lower-cases them all before returning -- no BaseSchema/Annotated return type
    // anywhere. pipeline.py, in a different file, only sees the lower-cased names.
    // The cross-file return-schema mechanism must pick up the POST-fold schema
    // (not the raw upper-cased Snowflake one) when resolving load_orders' return
    // type at pipeline.py's call site.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(
        root.join("pyproject.toml"),
        "[tool.typedframes]\nsql_dialect = \"snowflake\"\n",
    )
    .unwrap();
    fs::write(
        root.join("loaders.py"),
        r#"
import pandas as pd
import internal_snowflake_pkg

def load_orders(query: str) -> pd.DataFrame:
    conn = internal_snowflake_pkg.connect()
    cursor = conn.cursor()
    cursor.execute("SELECT order_id, amount FROM orders")
    df = cursor.fetch_pandas_all()
    df.columns = df.columns.str.lower()
    return df
"#,
    )
    .unwrap();
    let pipeline_source = r#"
from loaders import load_orders

def process(query: str) -> None:
    orders = load_orders(query)
    print(orders["order_id"])
    print(orders["ORDER_ID"])
"#;
    fs::write(root.join("pipeline.py"), pipeline_source).unwrap();

    // act
    let index = build_index_internal(root);
    let mut linter = Linter::new();
    linter.fs = Arc::new(OsFiles).into();
    linter.with_context(
        root.to_path_buf(),
        &LinterConfig {
            enabled: None,
            warnings: None,
            sql_dialect: Some("snowflake".to_string()),
            trace_external_packages: None,
            excluded_external_packages: None,
            exclude: None,
        },
    );
    let pipeline_path = root.join("pipeline.py");
    linter.load_cross_file_symbols(&index, pipeline_source, &pipeline_path, root);
    let errors = linter
        .check_file_internal(pipeline_source, &pipeline_path)
        .unwrap();

    // assert
    assert_eq!(errors.len(), 1, "expected one error, got: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
    assert!(errors[0].message.contains("ORDER_ID"));
}

#[test]
fn test_should_trace_an_explicitly_allowlisted_package_installed_in_venv_site_packages() {
    // arrange: the case-folding helper genuinely lives OUTSIDE the project tree,
    // in a fake .venv/lib/pythonX.Y/site-packages/internal_snowflake_pkg/ --
    // simulating a real pip-installed internal package, not just another
    // first-party file. `trace_external_packages` forces it to be indexed
    // regardless of whether the default on-demand candidate detection would also
    // have caught it (see test_should_auto_trace_an_external_package_used_in_a_dataframe_shaped_way_by_default
    // for that case) -- the post-fold return schema must propagate across that
    // install boundary either way.
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
    df.columns = df.columns.str.lower()
    return df
"#,
    )
    .unwrap();

    let pipeline_source = r#"
from internal_snowflake_pkg import load_orders

def process(query: str) -> None:
    orders = load_orders(query)
    print(orders["order_id"])
    print(orders["ORDER_ID"])
"#;
    fs::write(root.join("pipeline.py"), pipeline_source).unwrap();

    let config_with_allowlist = LinterConfig {
        enabled: None,
        warnings: None,
        sql_dialect: Some("snowflake".to_string()),
        trace_external_packages: Some(vec!["internal_snowflake_pkg".to_string()]),
        excluded_external_packages: None,
        exclude: None,
    };

    // act: with the allowlist, the external package is indexed and its post-fold
    // return schema is picked up at pipeline.py's call site.
    fs::write(
            root.join("pyproject.toml"),
            "[tool.typedframes]\nsql_dialect = \"snowflake\"\ntrace_external_packages = [\"internal_snowflake_pkg\"]\n",
        )
        .unwrap();
    let index_with_allowlist = build_index_internal(root);
    let mut linter_with = Linter::new();
    linter_with.with_context(root.to_path_buf(), &config_with_allowlist);
    let pipeline_path = root.join("pipeline.py");
    linter_with.load_cross_file_symbols(
        &index_with_allowlist,
        pipeline_source,
        &pipeline_path,
        root,
    );
    let errors_with = linter_with
        .check_file_internal(pipeline_source, &pipeline_path)
        .unwrap();

    // assert
    assert_eq!(
        errors_with.len(),
        1,
        "expected one error, got: {errors_with:?}"
    );
    assert_eq!(errors_with[0].code, CODE_UNKNOWN_COLUMN);
    assert!(errors_with[0].message.contains("ORDER_ID"));
}

#[test]
fn test_should_auto_trace_an_external_package_used_in_a_dataframe_shaped_way_by_default() {
    // arrange: the exact same fixture as
    // test_should_trace_an_explicitly_allowlisted_package_installed_in_venv_site_packages,
    // but with NO `trace_external_packages` configured at all. `load_orders`'s
    // result (`orders`) is subscripted in pipeline.py -- a DataFrame-shaped usage
    // of an unresolved call to a plainly-imported external package -- which must
    // be enough to auto-discover and trace `internal_snowflake_pkg` by default.
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
    df.columns = df.columns.str.lower()
    return df
"#,
    )
    .unwrap();

    let pipeline_source = r#"
from internal_snowflake_pkg import load_orders

def process(query: str) -> None:
    orders = load_orders(query)
    print(orders["order_id"])
    print(orders["ORDER_ID"])
"#;
    fs::write(root.join("pipeline.py"), pipeline_source).unwrap();
    fs::write(
        root.join("pyproject.toml"),
        "[tool.typedframes]\nsql_dialect = \"snowflake\"\n",
    )
    .unwrap();

    let config = LinterConfig {
        enabled: None,
        warnings: None,
        sql_dialect: Some("snowflake".to_string()),
        trace_external_packages: None,
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

    // assert
    assert_eq!(errors.len(), 1, "expected one error, got: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
    assert!(errors[0].message.contains("ORDER_ID"));
}

#[test]
fn test_should_resolve_self_attr_method_call_to_a_first_party_class_in_another_file() {
    // arrange: DataRepository (with its schema) lives entirely in repository.py;
    // pipeline.py only imports the class and calls self._data_repository.get_
    // training() -- neither the class definition NOR TrainingSchema's column
    // list are visible without cross-file resolution (IndexEntry.class_methods,
    // consulted via Linter::import_name).
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(
        root.join("repository.py"),
        r#"
from typing import Annotated
import pandas as pd
from typedframes import BaseSchema, Column


class TrainingSchema(BaseSchema):
    feature_a = Column(type=float)
    label = Column(type=int)


class DataRepository:
    def get_training(self) -> Annotated[pd.DataFrame, TrainingSchema]:
        return pd.read_csv("training.csv")
"#,
    )
    .unwrap();
    let pipeline_source = r#"
from repository import DataRepository


class Pipeline:
    def __init__(self):
        self._data_repository = DataRepository()

    def run(self):
        df = self._data_repository.get_training()
        print(df["feature_a"])
        print(df["nonexistent"])
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

    // assert: the error message's schema location points back to repository.py,
    // proving TrainingSchema's columns (not just its name) crossed the file
    // boundary too, not only the resolved schema NAME.
    assert_eq!(errors.len(), 1, "expected one error, got: {errors:?}");
    assert!(errors[0].message.contains("nonexistent"));
    assert!(errors[0].message.contains("TrainingSchema"));
    assert!(
        errors[0].message.contains("repository.py"),
        "{}",
        errors[0].message
    );
    assert_eq!(linter.dataframes_total, 1);
    assert_eq!(linter.dataframes_typed, 1);
}

#[test]
fn test_should_trace_a_py_typed_external_package_via_self_attr_without_dataframe_shaped_usage() {
    // arrange: internal_repo_pkg declares py.typed, and its DataRepository.get is
    // resolved through self._data_repository -- but its result is NEVER
    // subscripted or otherwise used like a DataFrame anywhere in pipeline.py.
    // Ordinary auto-discovery (dataframe_shaped_usage) would never trigger on
    // this; py.typed alone must be enough.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let site_packages = root
        .join(".venv")
        .join("lib")
        .join("python3.12")
        .join("site-packages");
    let pkg_dir = site_packages.join("internal_repo_pkg");
    fs::create_dir_all(&pkg_dir).unwrap();
    fs::write(pkg_dir.join("py.typed"), "").unwrap();
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

    // Orchestrator gives Pipeline.run() a real call site -- required so its
    // count isn't reversed with nowhere to land; see
    // test_should_count_typed_dataframe_for_load_call_inside_if_block's comment
    // on why. `go`'s own body must NOT itself do anything DataFrame-shaped with
    // `result` (no subscript, no reserved-method call) -- this test is
    // specifically about py.typed tracing working WITHOUT dataframe_shaped_usage.
    let pipeline_source = r#"
from internal_repo_pkg import DataRepository


class Pipeline:
    def __init__(self):
        self._data_repository = DataRepository()

    def run(self):
        df = self._data_repository.get("SELECT * FROM training")
        return df


class Orchestrator:
    def __init__(self):
        self._pipeline = Pipeline()

    def go(self):
        result = self._pipeline.run()
        print(result)
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

    // assert: recognized as a DataFrame (bare pd.DataFrame, no attached Schema),
    // but NOT counted as typed -- no column name could ever be validated against
    // it, so it lands in untyped_sites like any other unresolved origin. The
    // count is attributed to Orchestrator.go's call site (`result`), not to
    // Pipeline.run's own internal `df` -- Pipeline.run is a pure pass-through, so
    // its count moves to the call site that actually resolves it, same as every
    // other pass-through in the journey/leg model.
    assert_eq!(linter.dataframes_total, 1);
    assert_eq!(linter.dataframes_typed, 0);
    assert_eq!(linter.untyped_sites.len(), 1);
    assert_eq!(linter.untyped_sites[0].var, "result");
}

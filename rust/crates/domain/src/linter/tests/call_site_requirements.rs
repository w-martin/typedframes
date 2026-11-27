use super::*;

#[test]
fn test_should_detect_missing_column_at_direct_call_site() {
    // arrange: postproc requires "c" directly off its own parameter; the caller's
    // loader only provides {a, b}.
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
from loaders import load
from steps import postproc

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
    assert!(errors[0].message.contains("available: {a, b}"));
    assert!(errors[0].message.contains("required: {c}"));
    assert!(errors[0].message.contains("passed to postproc"));
}

#[test]
fn test_should_confirm_pass_through_reversal_across_files() {
    // arrange: loaders.py's `load` is a pure pass-through (`df = pd.read_csv(...);
    // return df`), called only from pipeline.py -- a DIFFERENT file, never from
    // within loaders.py itself. Regression coverage for the cross-file double-count
    // this closes: before `ProjectIndex.called_functions` existed, loaders.py's own
    // per-file check had no way to know anyone calls `load`, so it kept counting
    // its internal origin (1) *in addition to* pipeline.py's own call-site count
    // (1) -- two counts, project-wide, for what is really one DataFrame.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    let loaders_source = r#"
import pandas as pd

def load(path: str) -> pd.DataFrame:
    df = pd.read_csv(path, usecols=["a", "b"])
    return df
"#;
    fs::write(root.join("loaders.py"), loaders_source).unwrap();
    let pipeline_source = r#"
from loaders import load

def process(path: str) -> None:
    df = load(path)
    print(df)
"#;
    fs::write(root.join("pipeline.py"), pipeline_source).unwrap();

    // act
    let index = build_index_internal(root);

    let mut loaders_linter = Linter::new();
    let loaders_path = root.join("loaders.py");
    loaders_linter.load_cross_file_symbols(&index, loaders_source, &loaders_path, root);
    loaders_linter
        .check_file_internal(loaders_source, &loaders_path)
        .unwrap();

    let mut pipeline_linter = Linter::new();
    let pipeline_path = root.join("pipeline.py");
    pipeline_linter.load_cross_file_symbols(&index, pipeline_source, &pipeline_path, root);
    pipeline_linter
        .check_file_internal(pipeline_source, &pipeline_path)
        .unwrap();

    // assert: loaders.py's own internal origin was reversed (confirmed called from
    // pipeline.py, via ProjectIndex.called_functions) -- its own per-file tally is
    // now 0, not 1. pipeline.py's call site is the one and only count.
    assert_eq!(loaders_linter.dataframes_total, 0);
    assert_eq!(loaders_linter.dataframes_typed, 0);
    assert_eq!(pipeline_linter.dataframes_total, 1);
    assert_eq!(pipeline_linter.dataframes_typed, 1);
}

#[test]
fn test_should_confirm_pass_through_reversal_across_files_via_self_attr() {
    // arrange: the `self.<attr>.<method>()` counterpart to
    // test_should_confirm_pass_through_reversal_across_files -- Warehouse.fetch_orders
    // is a pure pass-through, called only from reporting.py's Reporter class, via
    // `self._warehouse.fetch_orders()`, never from within warehouse.py itself.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    let warehouse_source = r#"
import pandas as pd


class Warehouse:
    def fetch_orders(self) -> pd.DataFrame:
        df = pd.read_csv("orders.csv", usecols=["a", "b"])
        return df
"#;
    fs::write(root.join("warehouse.py"), warehouse_source).unwrap();
    let reporting_source = r#"
from warehouse import Warehouse


class Reporter:
    def __init__(self):
        self._warehouse = Warehouse()

    def summarize(self):
        orders = self._warehouse.fetch_orders()
        print(orders)
"#;
    fs::write(root.join("reporting.py"), reporting_source).unwrap();

    // act
    let index = build_index_internal(root);

    let mut warehouse_linter = Linter::new();
    let warehouse_path = root.join("warehouse.py");
    warehouse_linter.load_cross_file_symbols(&index, warehouse_source, &warehouse_path, root);
    warehouse_linter
        .check_file_internal(warehouse_source, &warehouse_path)
        .unwrap();

    let mut reporting_linter = Linter::new();
    let reporting_path = root.join("reporting.py");
    reporting_linter.load_cross_file_symbols(&index, reporting_source, &reporting_path, root);
    reporting_linter
        .check_file_internal(reporting_source, &reporting_path)
        .unwrap();

    // assert: warehouse.py's own internal origin was reversed (confirmed called
    // from reporting.py's self._warehouse.fetch_orders(), resolved via
    // ProjectIndex.called_functions' self-attr handling) -- its own tally is now
    // 0, not 1. reporting.py's call site is the one and only count.
    assert_eq!(warehouse_linter.dataframes_total, 0);
    assert_eq!(warehouse_linter.dataframes_typed, 0);
    assert_eq!(reporting_linter.dataframes_total, 1);
    assert_eq!(reporting_linter.dataframes_typed, 1);
}

#[test]
fn test_should_detect_missing_column_at_call_site_for_free_function() {
    // arrange: same-file baseline for the instance-method comparison test below --
    // contact_label's own body accesses {name, email} off its bare parameter, but
    // the loader only provides {customer_id, name}.
    let source = r#"
import pandas as pd
from typedframes import BaseSchema, Column

class CustomerSchema(BaseSchema):
    customer_id = Column(type=str)
    name = Column(type=str)

def contact_label(customers):
    return customers["name"] + customers["email"]

customers = pd.read_csv("customers.csv", usecols=["customer_id", "name"])
contact_label(customers)
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(errors.len(), 1, "expected one error, got: {errors:?}");
    assert_eq!(errors[0].code, "missing-column");
    assert!(errors[0].message.contains("passed to contact_label"));
}

#[test]
fn test_should_detect_missing_column_at_call_site_for_instance_method() {
    // arrange: identical to test_should_detect_missing_column_at_call_site_for_free_function
    // except contact_label is now an instance method (self, customers) -- the
    // shape SOLID/DI-style codebases actually use. The contract-inference pass
    // must taint `customers`, the real DataFrame parameter, not `self`.
    let source = r#"
import pandas as pd
from typedframes import BaseSchema, Column

class CustomerSchema(BaseSchema):
    customer_id = Column(type=str)
    name = Column(type=str)

class Labeler:
    def contact_label(self, customers):
        return customers["name"] + customers["email"]

customers = pd.read_csv("customers.csv", usecols=["customer_id", "name"])
labeler = Labeler()
labeler.contact_label(customers)
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(errors.len(), 1, "expected one error, got: {errors:?}");
    assert_eq!(errors[0].code, "missing-column");
    assert!(errors[0].message.contains("passed to contact_label"));
}

#[test]
fn test_should_resolve_transitive_requires_through_delegate_chain() {
    // arrange: transform has no direct subscript access of its own — it only
    // delegates df through preproc -> infer -> postproc, which together need
    // {a, b, c}. The loader only supplies {a, b}, so 'c' should surface as a
    // missing-column error at the `transform(df)` call site in pipeline.py,
    // even though 'c' is only referenced two hops down, inside postproc.
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

def preproc(df: pd.DataFrame) -> pd.DataFrame:
    z = df["a"]
    return df

def infer(df: pd.DataFrame) -> pd.DataFrame:
    x = df["b"]
    return df

def postproc(df: pd.DataFrame) -> pd.DataFrame:
    y = df["c"]
    return df

def transform(df: pd.DataFrame) -> pd.DataFrame:
    step1 = preproc(df)
    step2 = infer(step1)
    step3 = postproc(step2)
    return step3
"#,
    )
    .unwrap();
    let pipeline_source = r#"
from loaders import load
from steps import transform

def process(path: str) -> None:
    df = load(path)
    result = transform(df)
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
    assert!(errors[0].message.contains("required: {a, b, c}"));
    assert!(errors[0].message.contains("passed to transform"));
}

#[test]
fn test_should_not_flag_call_when_all_required_columns_present() {
    // arrange: same delegate chain as above, but the loader supplies {a, b, c} —
    // a strict superset of everything preproc/infer/postproc need.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    fs::write(
        root.join("loaders.py"),
        r#"
import pandas as pd

def load(path: str) -> pd.DataFrame:
    df = pd.read_csv(path, usecols=["a", "b", "c"])
    return df
"#,
    )
    .unwrap();
    fs::write(
        root.join("steps.py"),
        r#"
import pandas as pd

def preproc(df: pd.DataFrame) -> pd.DataFrame:
    z = df["a"]
    return df

def infer(df: pd.DataFrame) -> pd.DataFrame:
    x = df["b"]
    return df

def postproc(df: pd.DataFrame) -> pd.DataFrame:
    y = df["c"]
    return df

def transform(df: pd.DataFrame) -> pd.DataFrame:
    step1 = preproc(df)
    step2 = infer(step1)
    step3 = postproc(step2)
    return step3
"#,
    )
    .unwrap();
    let pipeline_source = r#"
from loaders import load
from steps import transform

def process(path: str) -> None:
    df = load(path)
    result = transform(df)
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
    assert!(errors.is_empty(), "expected no errors, got: {errors:?}");
}

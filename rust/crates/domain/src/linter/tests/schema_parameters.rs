use super::*;

#[test]
fn test_should_validate_schema_annotated_parameter_in_body() {
    // arrange: contact_label's parameter is explicitly annotated with
    // CustomerSchema, which does not declare "email" — the bad access should
    // be caught inside the function body itself, standalone, no caller or
    // project index involved.
    let source = r#"
from typedframes import BaseSchema, Column

class CustomerSchema(BaseSchema):
    customer_id = Column(type=int)
    name = Column(type=str)

def contact_label(customers: DataFrame[CustomerSchema]):
    print(customers["name"])
    print(customers["email"])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(errors.len(), 1, "expected one error, got: {errors:?}");
    assert!(errors[0].message.contains("email"));
    assert!(errors[0].message.contains("CustomerSchema"));
}

#[test]
fn test_should_use_schema_annotation_as_call_site_requirement() {
    // arrange: contact_label's parameter is annotated with CustomerSchema, which
    // declares {customer_id, name, email} — but the body only ever subscripts
    // "name". The call-site contract must still require all three columns (the
    // annotation is authoritative), not just the one the body happens to touch,
    // so a caller missing "email" is flagged even though nothing in
    // contact_label's body literally writes customers["email"].
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    fs::write(
        root.join("loaders.py"),
        r#"
import pandas as pd

def load(path: str) -> pd.DataFrame:
    df = pd.read_csv(path, usecols=["customer_id", "name"])
    return df
"#,
    )
    .unwrap();
    fs::write(
        root.join("transforms.py"),
        r#"
from typedframes import BaseSchema, Column

class CustomerSchema(BaseSchema):
    customer_id = Column(type=int)
    name = Column(type=str)
    email = Column(type=str)

def contact_label(customers: DataFrame[CustomerSchema]):
    print(customers["name"])
    return customers
"#,
    )
    .unwrap();
    let pipeline_source = r#"
from loaders import load
from transforms import contact_label

def process(path: str) -> None:
    customers = load(path)
    contact_label(customers)
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
    assert!(errors[0].message.contains("missing column(s) {email}"));
    assert!(errors[0]
        .message
        .contains("required: {customer_id, email, name}"));
}

#[test]
fn test_should_validate_cross_file_schema_annotated_parameter_in_body() {
    // arrange: CustomerSchema lives in its own schemas.py, imported into
    // transforms.py and used as a parameter annotation there — confirms that
    // in-body validation (fix a) works across a file boundary too, not just
    // when the schema is defined in the same file as the function using it.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    fs::write(
        root.join("schemas.py"),
        r#"
from typedframes import BaseSchema, Column

class CustomerSchema(BaseSchema):
    customer_id = Column(type=int)
    name = Column(type=str)
    email = Column(type=str)
"#,
    )
    .unwrap();
    let transforms_source = r#"
from schemas import CustomerSchema

def contact_label(customers: DataFrame[CustomerSchema]):
    print(customers["name"])
    print(customers["not_a_real_column"])
    return customers
"#;
    fs::write(root.join("transforms.py"), transforms_source).unwrap();

    // act: check transforms.py itself, as part of a directory scan (with index)
    let index = build_index_internal(root);
    let mut linter = Linter::new();
    let transforms_path = root.join("transforms.py");
    linter.load_cross_file_symbols(&index, transforms_source, &transforms_path, root);
    let errors = linter
        .check_file_internal(transforms_source, &transforms_path)
        .unwrap();

    // assert
    assert_eq!(errors.len(), 1, "expected one error, got: {errors:?}");
    assert_eq!(errors[0].code, "unknown-column");
    assert!(errors[0].message.contains("not_a_real_column"));
    assert!(errors[0].message.contains("CustomerSchema"));
}

#[test]
fn test_should_validate_variable_whose_schema_is_defined_in_a_third_file() {
    // arrange: schemas.py defines CustomerSchema; loaders.py's load_customers()
    // returns Annotated[pd.DataFrame, CustomerSchema] but does NOT itself define
    // or re-export CustomerSchema; pipeline.py imports only load_customers, never
    // CustomerSchema directly. Before the fix, self.schemas never learned
    // CustomerSchema's columns in pipeline.py's context (load_cross_file_symbols
    // only looked in load_customers' OWN file's entry, which doesn't have it), so
    // ANY access on `customers` silently skipped validation — no false positives,
    // but also no true positives. This must now be caught.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    fs::write(
        root.join("schemas.py"),
        r#"
from typedframes import BaseSchema, Column

class CustomerSchema(BaseSchema):
    customer_id = Column(type=int)
    name = Column(type=str)
"#,
    )
    .unwrap();
    fs::write(
        root.join("loaders.py"),
        r#"
from typing import Annotated
import pandas as pd
from schemas import CustomerSchema

def load_customers(path: str) -> Annotated[pd.DataFrame, CustomerSchema]:
    return pd.read_csv(path, usecols=["customer_id", "name"])
"#,
    )
    .unwrap();
    let pipeline_source = r#"
from loaders import load_customers

def process(path: str) -> None:
    customers = load_customers(path)
    print(customers["not_a_real_column"])
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
    assert_eq!(errors[0].code, "unknown-column");
    assert!(errors[0].message.contains("not_a_real_column"));
    assert!(errors[0].message.contains("CustomerSchema"));
}

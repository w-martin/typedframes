use super::*;

#[test]
fn test_should_lint_base_schema_column_access() {
    // arrange
    let source = r#"
from typedframes import BaseSchema, Column

class UserSchema(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)

df: DataFrame[UserSchema] = load()
print(df["user_id"])
print(df["name"])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(errors.len(), 1);
    assert!(errors[0].message.contains("name"));
    assert!(errors[0].message.contains("UserSchema"));
}

#[test]
fn test_should_catch_unknown_column_via_check_notebook_internal() {
    // Sanity check that switching the parse mode to Jupyter/IPython for notebooks
    // doesn't change behavior on ordinary magic-free source.
    let source = r#"
from typedframes import BaseSchema, Column

class UserSchema(BaseSchema):
    user_id = Column(type=int)

df: DataFrame[UserSchema] = load()
print(df["name"])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_notebook_internal(source, Path::new("nb.ipynb"))
        .unwrap();

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:#?}");
    assert!(errors[0].message.contains("name"));
}

#[test]
fn test_should_tolerate_line_magic_in_notebook_mode() {
    // A bare `parse_module` (Python-only mode) would fail outright on the
    // `%matplotlib inline` line; Jupyter/IPython mode tokenizes it as an
    // IpyEscapeCommand statement instead, and the real error below it is still
    // caught.
    let source = r#"
%matplotlib inline
from typedframes import BaseSchema, Column

class UserSchema(BaseSchema):
    user_id = Column(type=int)

df: DataFrame[UserSchema] = load()
print(df["name"])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_notebook_internal(source, Path::new("nb.ipynb"))
        .unwrap();

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:#?}");
    assert!(errors[0].message.contains("name"));
}

#[test]
fn test_should_tolerate_shell_escape_in_notebook_mode() {
    let source = r#"
!pip install pandas
from typedframes import BaseSchema, Column

class UserSchema(BaseSchema):
    user_id = Column(type=int)

df: DataFrame[UserSchema] = load()
print(df["name"])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_notebook_internal(source, Path::new("nb.ipynb"))
        .unwrap();

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:#?}");
}

#[test]
fn test_should_return_err_for_syntax_error_unrelated_to_magics_in_notebook_mode() {
    // A genuine Python syntax error (not a magic/shell escape) must still be
    // reported as a parse failure, same as check_file_internal would for a .py file.
    let source = "def f(:\n    pass\n";
    let mut linter = Linter::new();

    // act
    let result = linter.check_notebook_internal(source, Path::new("nb.ipynb"));

    // assert
    assert!(result.is_err());
}

#[test]
fn test_should_resolve_cross_file_schema_import_past_a_leading_magic() {
    // A magic line before the `from schemas import ...` statement must not
    // prevent cross-file symbol loading for the rest of the notebook -- this is
    // exactly the case load_cross_file_symbols_notebook exists to handle (a plain
    // parse_module on this source would fail outright on the magic line and
    // silently skip cross-file resolution for the whole file).
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    fs::write(
        root.join("schemas.py"),
        r#"
from typedframes import BaseSchema, Column

class UserSchema(BaseSchema):
    user_id = Column(type=int)
"#,
    )
    .unwrap();
    let notebook_source = r#"
%matplotlib inline
from schemas import UserSchema

df: DataFrame[UserSchema] = load()
print(df["name"])
"#;
    let notebook_path = root.join("analysis.ipynb");

    // act
    let index = build_index_internal(root);
    let mut linter = Linter::new();
    linter.load_cross_file_symbols_notebook(&index, notebook_source, root);
    let errors = linter
        .check_notebook_internal(notebook_source, &notebook_path)
        .unwrap();

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:#?}");
    assert!(
        errors[0].message.contains("user_id"),
        "{}",
        errors[0].message
    );
}

#[test]
fn test_should_list_available_columns_for_a_named_schema_unknown_column_error() {
    // Named schemas (BaseSchema subclasses) used to only get "SchemaName (defined
    // at line N)" in an unknown-column message -- no column list, and that line was
    // actually the VARIABLE's binding line, not the schema class's own definition.
    // schema_display now includes the column set for named schemas too, and points
    // at the class's actual definition (line 4, where `class UserSchema` is) rather
    // than the variable binding (line 7).
    let source = r#"
from typedframes import BaseSchema, Column

class UserSchema(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)

df: DataFrame[UserSchema] = load()
print(df["name"])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(errors.len(), 1);
    assert!(
        errors[0].message.contains("user_id"),
        "{}",
        errors[0].message
    );
    assert!(errors[0].message.contains("email"), "{}", errors[0].message);
    assert!(
        errors[0].message.contains("defined at test.py:4"),
        "should point at the class definition (line 4), not the variable binding \
             (line 7): {}",
        errors[0].message
    );
}

#[test]
fn test_should_point_unknown_column_message_at_schema_defined_in_a_different_file() {
    // A named schema imported from another file must still resolve its own
    // class-definition location cross-file, not just its column list -- the
    // schema's own file, not the importing pipeline file, is what belongs in
    // "defined at ...".
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    fs::write(
        root.join("schemas.py"),
        r#"
from typedframes import BaseSchema, Column

class UserSchema(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)
"#,
    )
    .unwrap();
    let pipeline_source = r#"
from schemas import UserSchema

df: DataFrame[UserSchema] = load()
print(df["name"])
"#;
    let pipeline_path = root.join("pipeline.py");
    fs::write(&pipeline_path, pipeline_source).unwrap();

    // act
    let index = build_index_internal(root);
    let mut linter = Linter::new();
    linter.load_cross_file_symbols(&index, pipeline_source, &pipeline_path, root);
    let errors = linter
        .check_file_internal(pipeline_source, &pipeline_path)
        .unwrap();

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:#?}");
    let schemas_path = root.join("schemas.py");
    let expected = format!("defined at {}:4", schemas_path.to_str().unwrap());
    assert!(
        errors[0].message.contains(&expected),
        "expected {:?} in {}",
        expected,
        errors[0].message
    );
}

#[test]
fn test_should_list_available_columns_in_mutation_tracking_error() {
    // df["new_col"] = ... on a named-schema-tracked variable, where new_col isn't
    // in the schema, is flagged (mutation tracking) -- that message used to bypass
    // schema_display entirely and print only the bare schema name, with no column
    // list and no class-definition location. Now routes through schema_display
    // like every other unknown-column message, pointing at the schema's own class
    // definition (line 4, where `class UserSchema` is), not the mutating line.
    let source = r#"
from typedframes import BaseSchema, Column

class UserSchema(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)

df: DataFrame[UserSchema] = load()
df["new_column"] = 1
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:#?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
    assert!(errors[0].message.contains("mutation tracking"));
    assert!(
        errors[0].message.contains("user_id"),
        "{}",
        errors[0].message
    );
    assert!(errors[0].message.contains("email"), "{}", errors[0].message);
    assert!(
        errors[0].message.contains("defined at test.py:4"),
        "{}",
        errors[0].message
    );
}

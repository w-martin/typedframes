use super::*;

#[test]
fn test_should_print_help_and_exit_2_without_a_command() {
    // arrange
    let project = Project::new();

    // act
    let output = project.run(&[]);

    // assert
    assert_eq!(2, output.code);
    assert!(output
        .stdout
        .contains("Static analysis for DataFrame column schemas."));
}

#[test]
fn test_should_report_a_clean_project_and_exit_0() {
    // arrange
    let project = Project::new();
    project.write("ok.py", "x = 1\n");

    // act
    let output = project.run(&["check", "."]);

    // assert
    assert_eq!(0, output.code);
    assert!(
        output.stdout.starts_with("\u{2713} Checked 1 file in "),
        "{}",
        output.stdout
    );
    assert!(output
        .stdout
        .contains("No DataFrames with recognized loads/schemas found to check"));
}

#[test]
fn test_should_print_an_error_line_and_exit_0_unless_strict() {
    // arrange
    let project = Project::new();
    project.write("bad.py", MISSING_COLUMN);

    // act
    let lenient = project.run(&["check", "."]);
    let strict = project.run(&["check", ".", "--strict"]);

    // assert
    assert_eq!(0, lenient.code);
    assert_eq!(1, strict.code);
    assert!(
        lenient
            .stdout
            .contains("bad.py:14:7: error[unknown-column]"),
        "{}",
        lenient.stdout
    );
    assert!(lenient
        .stdout
        .contains("\u{2717} Found 1 error in 1 file ("));
}

#[test]
fn test_should_emit_one_json_document_with_errors_and_stats() {
    // arrange
    let project = Project::new();
    project.write("bad.py", MISSING_COLUMN);

    // act
    let output = project.run(&["check", ".", "--output-format", "json"]);

    // assert
    let document: Value = serde_json::from_str(&output.stdout).unwrap();
    assert_eq!("unknown-column", document["errors"][0]["code"]);
    assert!(document["errors"][0]["file"]
        .as_str()
        .unwrap()
        .ends_with("bad.py"));
    assert_eq!(1, document["stats"]["dataframes_total"]);
    assert_eq!(1, document["stats"]["dataframes_typed"]);
}

#[test]
fn test_should_escape_non_ascii_in_json_output_like_python_did() {
    // arrange
    let project = Project::new();
    project.write(
        "bad.py",
        &MISSING_COLUMN.replace("non_existent", "na\u{ef}ve"),
    );

    // act
    let output = project.run(&["check", ".", "--json"]);

    // assert
    assert!(output.stdout.contains("na\\u00efve"), "{}", output.stdout);
    assert!(output.stdout.starts_with("{\n  \"errors\": ["));
}

#[test]
fn test_should_emit_github_annotations() {
    // arrange
    let project = Project::new();
    project.write("bad.py", MISSING_COLUMN);

    // act
    let output = project.run(&["check", ".", "--output-format", "github", "--no-info"]);

    // assert
    assert!(
        output.stdout.starts_with("::error file="),
        "{}",
        output.stdout
    );
    assert!(output
        .stdout
        .contains(",line=14,col=7,title=unknown-column::"));
}

#[test]
fn test_should_exit_2_and_say_so_for_a_path_that_does_not_exist() {
    // arrange
    let project = Project::new();

    // act
    let output = project.run(&["check", "missing/dir"]);

    // assert
    assert_eq!(2, output.code);
    assert!(output
        .stderr
        .contains("Error: path does not exist: PosixPath('missing/dir') (resolved to "));
}

#[test]
fn test_should_skip_the_default_excluded_directories_and_a_file_that_does_not_parse() {
    // arrange
    let project = Project::new();
    project.write("src/bad.py", MISSING_COLUMN);
    project.write("build/ignored.py", MISSING_COLUMN);
    project.write("src/broken.py", "def f(:\n    pass\n");

    // act
    let output = project.run(&["check", "."]);

    // assert
    assert_eq!(0, output.code);
    assert!(
        output.stdout.contains("Found 1 error in 2 files"),
        "{}",
        output.stdout
    );
    assert!(
        output.stderr.contains("broken.py: skipped, "),
        "{}",
        output.stderr
    );
}

#[test]
fn test_should_let_configured_excludes_replace_the_default_set() {
    // arrange
    let project = Project::new();
    project.write(
        "pyproject.toml",
        "[tool.typedframes]\nexclude = [\"legacy\"]\n",
    );
    project.write("legacy/old.py", MISSING_COLUMN);
    project.write("build/kept.py", MISSING_COLUMN);

    // act
    let output = project.run(&["check", "."]);

    // assert
    assert!(output.stdout.contains("kept.py"), "{}", output.stdout);
    assert!(!output.stdout.contains("old.py"), "{}", output.stdout);
}

#[test]
fn test_should_check_nothing_when_the_project_disables_the_checker() {
    // arrange
    let project = Project::new();
    project.write("pyproject.toml", "[tool.typedframes]\nenabled = false\n");
    project.write("bad.py", MISSING_COLUMN);

    // act
    let output = project.run(&["check", ".", "--strict"]);

    // assert
    assert_eq!(0, output.code);
    assert!(output.stdout.contains("\u{2713} Checked 1 file"));
}

#[test]
fn test_should_resolve_a_schema_defined_in_another_file() {
    // arrange
    let project = Project::new();
    project.write(
        "schemas.py",
        "from typedframes import BaseSchema, Column\n\n\nclass UserSchema(BaseSchema):\n    user_id = Column(type=int)\n",
    );
    project.write(
        "pipeline.py",
        "from schemas import UserSchema\n\ndf: DataFrame[UserSchema] = load()\nprint(df[\"name\"])\n",
    );

    // act
    let output = project.run(&["check", "pipeline.py"]);

    // assert
    assert!(
        output.stdout.contains("unknown-column"),
        "{}",
        output.stdout
    );
    assert!(output.stdout.contains("defined at"), "{}", output.stdout);
}

#[test]
fn test_should_exit_1_when_coverage_is_below_the_command_line_threshold() {
    // arrange
    let project = Project::new();
    project.write(
        "a.py",
        "import pandas as pd\ndf = pd.read_csv(\"o.csv\")\nprint(df[\"z\"])\n",
    );

    // act
    let output = project.run(&["check", ".", "--coverage-fail-under", "100", "--no-info"]);

    // assert
    assert_eq!(1, output.code);
    assert!(
        output.stdout.contains("\u{2717} DataFrame schema coverage 0.0% is below the required 100.0% (0/1 DataFrames had column info)"),
        "{}",
        output.stdout
    );
}

#[test]
fn test_should_grade_a_subtree_on_its_own_override_from_the_project_file() {
    // arrange
    let project = Project::new();
    project.write(
        "pyproject.toml",
        "[tool.typedframes.coverage]\nenabled = true\nfail_under = 100\n[tool.typedframes.coverage.overrides]\n\"legacy/**\" = 0\n",
    );
    project.write(
        "legacy/old.py",
        "import pandas as pd\ndf = pd.read_csv(\"o.csv\")\nprint(df[\"z\"])\n",
    );

    // act
    let output = project.run(&["check", "."]);

    // assert
    assert_eq!(0, output.code, "{}", output.stdout);
}

#[test]
fn test_should_print_the_term_missing_table_for_untyped_sites() {
    // arrange
    let project = Project::new();
    project.write(
        "a.py",
        "import pandas as pd\ndf = pd.read_csv(\"o.csv\")\nprint(df[\"z\"])\n",
    );

    // act
    let output = project.run(&["check", ".", "--coverage-detail", "term-missing"]);

    // assert
    assert!(
        output
            .stdout
            .contains("Name  Typed  Total   Cover   Missing"),
        "{}",
        output.stdout
    );
    assert!(
        output.stdout.contains("a.py      0      1      0%   df:2"),
        "{}",
        output.stdout
    );
}

#[test]
fn test_should_label_a_notebook_error_with_its_cell() {
    // arrange
    let project = Project::new();
    let cells = vec![serde_json::json!({
        "cell_type": "code", "metadata": {}, "execution_count": null, "outputs": [],
        "source": MISSING_COLUMN.split_inclusive('\n').collect::<Vec<_>>(),
    })];
    let notebook = serde_json::json!({
        "cells": cells,
        "metadata": {"kernelspec": {"display_name": "Python 3", "language": "python", "name": "python3"},
                     "language_info": {"name": "python"}},
        "nbformat": 4, "nbformat_minor": 5,
    });
    project.write("n.ipynb", &notebook.to_string());

    // act
    let output = project.run(&["check", "."]);

    // assert
    assert!(
        output
            .stdout
            .contains("n.ipynb:cell 1:14:7: error[unknown-column]"),
        "{}",
        output.stdout
    );
}

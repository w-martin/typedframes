use super::*;

pub(super) const ROWS_SCHEMA: &str = "from typedframes import BaseSchema, Column\n\n\nclass Rows(BaseSchema):\n    a = Column(type=int)\n    b = Column(type=int)\n";

#[test]
fn test_should_check_a_frame_whose_schema_is_imported_under_type_checking() {
    // arrange
    let project = Project::new();
    project.write("schema.py", ROWS_SCHEMA);
    project.write(
        "use.py",
        "from typing import TYPE_CHECKING, Annotated\n\nimport pandas as pd\n\nif TYPE_CHECKING:\n    from schema import Rows\n\n\ndef f(raw):\n    df: Annotated[pd.DataFrame, Rows] = pd.DataFrame(raw)\n    return df[\"NoSuchColumn\"]\n",
    );

    // act
    let output = project.run(&["check", "."]);

    // assert
    assert!(
        output.stdout.contains("unknown-column"),
        "{}",
        output.stdout
    );
    assert!(
        output.stdout.contains("1/1 DataFrames had column info"),
        "{}",
        output.stdout
    );
}

#[test]
fn test_should_check_a_frame_whose_schema_is_imported_under_typing_type_checking_with_an_else() {
    // arrange
    let project = Project::new();
    project.write("schema.py", ROWS_SCHEMA);
    project.write(
        "use.py",
        "import typing\nfrom typing import Annotated\n\nimport pandas as pd\n\nif typing.TYPE_CHECKING:\n    from schema import Rows\nelse:\n    Rows = None\n\n\ndef f(raw):\n    df: Annotated[pd.DataFrame, Rows] = pd.DataFrame(raw)\n    return df[\"NoSuchColumn\"]\n",
    );

    // act
    let output = project.run(&["check", "."]);

    // assert
    assert!(
        output.stdout.contains("unknown-column"),
        "{}",
        output.stdout
    );
}

#[test]
fn test_should_report_a_frame_annotated_with_a_schema_that_resolves_nowhere() {
    // arrange
    let project = Project::new();
    project.write(
        "use.py",
        "from typing import Annotated\n\nimport pandas as pd\n\n\ndef f(raw):\n    df: Annotated[pd.DataFrame, Missing] = pd.DataFrame(raw)\n    return df[\"a\"]\n",
    );

    // act
    let output = project.run(&["check", "."]);

    // assert
    assert!(
        output.stdout.contains("unresolved-schema"),
        "{}",
        output.stdout
    );
    assert!(
        output
            .stdout
            .contains("0/1 DataFrames had column info (0%), 1 annotated with an unresolved schema"),
        "{}",
        output.stdout
    );
}

#[test]
fn test_should_report_a_parameter_annotated_with_a_schema_that_resolves_nowhere() {
    // arrange
    let project = Project::new();
    project.write(
        "use.py",
        "from typing import Annotated\n\nimport pandas as pd\n\n\ndef f(p: Annotated[pd.DataFrame, Missing]):\n    return p[\"a\"]\n",
    );

    // act
    let output = project.run(&["check", "."]);

    // assert
    assert!(
        output.stdout.contains("warning[unresolved-schema]"),
        "{}",
        output.stdout
    );
    assert!(!output.stdout.contains("error["), "{}", output.stdout);
}

#[test]
fn test_should_check_a_frame_against_a_schema_defined_below_the_function_that_uses_it() {
    // arrange
    let project = Project::new();
    project.write(
        "use.py",
        "from typing import Annotated\n\nimport pandas as pd\nfrom typedframes import BaseSchema, Column\n\n\ndef f(raw):\n    df: Annotated[pd.DataFrame, Later] = pd.DataFrame(raw)\n    return df[\"zz\"]\n\n\nclass Later(BaseSchema):\n    a = Column(type=int)\n",
    );

    // act
    let output = project.run(&["check", "."]);

    // assert
    assert!(
        !output.stdout.contains("unresolved-schema"),
        "{}",
        output.stdout
    );
    assert!(
        output.stdout.contains("unknown-column"),
        "{}",
        output.stdout
    );
    assert!(
        output.stdout.contains("1/1 DataFrames had column info"),
        "{}",
        output.stdout
    );
}

#[test]
fn test_should_exit_1_under_strict_when_a_schema_is_unresolved() {
    // arrange
    let project = Project::new();
    project.write(
        "use.py",
        "from typing import Annotated\n\nimport pandas as pd\n\n\ndef f(raw):\n    df: Annotated[pd.DataFrame, Missing] = pd.DataFrame(raw)\n    return df\n",
    );

    // act
    let lenient = project.run(&["check", "."]);
    let strict = project.run(&["check", ".", "--strict"]);

    // assert
    assert_eq!(0, lenient.code, "{}", lenient.stdout);
    assert_eq!(1, strict.code, "{}", strict.stdout);
}

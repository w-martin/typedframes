use super::*;

const HELPER_HEAD: &str = "from typing import Annotated, Callable, TypeVar\n\nimport pandas as pd\nfrom typedframes import BaseSchema, Column\n\nT = TypeVar(\"T\")\n\n\nclass Rows(BaseSchema):\n    a = Column(type=int)\n    b = Column(type=int)\n\n\n";

fn check_helper_source(body: &str, args: &[&str]) -> Output {
    let project = Project::new();
    project.write("use.py", &format!("{HELPER_HEAD}{body}"));
    let mut command = vec!["check", "."];
    command.extend_from_slice(args);
    project.run(&command)
}

const PLAIN_COPY: &str =
    "def plain(frame: pd.DataFrame) -> pd.DataFrame:\n    return frame.copy()\n\n\n";

#[test]
fn test_should_keep_the_columns_through_a_plain_annotated_helper_that_copies() {
    // arrange
    let body = format!(
        "{PLAIN_COPY}def f(raw: Annotated[pd.DataFrame, Rows]):\n    out = plain(raw)\n    return out[\"a\"]\n"
    );

    // act
    let output = check_helper_source(&body, &[]);

    // assert
    assert!(!output.stdout.contains("-column"), "{}", output.stdout);
}

#[test]
fn test_should_flag_a_missing_column_after_a_plain_annotated_helper_that_copies() {
    // arrange
    let body = format!(
        "{PLAIN_COPY}def f(raw: Annotated[pd.DataFrame, Rows]):\n    out = plain(raw)\n    return out[\"nonexistent\"]\n"
    );

    // act
    let output = check_helper_source(&body, &[]);

    // assert
    assert!(
        output.stdout.contains("error[unknown-column]"),
        "{}",
        output.stdout
    );
}

#[test]
fn test_should_see_a_column_a_helper_adds_to_its_own_copy() {
    // arrange
    let body = "def adds(frame: pd.DataFrame) -> pd.DataFrame:\n    out = frame.copy()\n    out[\"new\"] = 1\n    return out\n\n\ndef f(raw: Annotated[pd.DataFrame, Rows]):\n    out = adds(raw)\n    return out[\"new\"], out[\"nope\"]\n";

    // act
    let output = check_helper_source(body, &[]);

    // assert
    assert!(
        output
            .stdout
            .contains("Column 'nope' does not exist in inferred column set {a, b, new}"),
        "{}",
        output.stdout
    );
    assert!(!output.stdout.contains("Column 'new'"), "{}", output.stdout);
}

#[test]
fn test_should_never_report_unknown_column_after_a_helper_that_loops_over_runtime_names() {
    // arrange
    let body = "def loops(frame: pd.DataFrame, names: list[str]) -> pd.DataFrame:\n    out = frame.copy()\n    for c in names:\n        out[c] = 1\n    return out\n\n\ndef f(raw: Annotated[pd.DataFrame, Rows], names: list[str]):\n    out = loops(raw, names)\n    return out[\"elsewhere\"]\n";

    // act
    let output = check_helper_source(body, &[]);

    // assert
    assert!(
        output.stdout.contains("warning[unverifiable-column]"),
        "{}",
        output.stdout
    );
    assert!(
        !output.stdout.contains("unknown-column"),
        "{}",
        output.stdout
    );
}

const FORWARDER: &str = "def typed(frame: Annotated[pd.DataFrame, Rows]) -> Annotated[pd.DataFrame, Rows]:\n    return frame\n\n\ndef run(name: str, fn: Callable[..., T], *args) -> T:\n    return fn(*args)\n\n\n";

#[test]
fn test_should_follow_a_forwarder_to_a_statically_known_function() {
    // arrange
    let body = format!(
        "{FORWARDER}def f(raw: Annotated[pd.DataFrame, Rows]):\n    out = run(\"stage\", typed, raw)\n    return out[\"nonexistent\"]\n"
    );

    // act
    let output = check_helper_source(&body, &[]);

    // assert
    assert!(
        output.stdout.contains("error[unknown-column]"),
        "{}",
        output.stdout
    );
}

#[test]
fn test_should_leave_a_forwarded_lambda_untracked_and_say_why() {
    // arrange
    let body = format!(
        "{FORWARDER}def f(raw: Annotated[pd.DataFrame, Rows]):\n    out = run(\"stage\", lambda frame: frame, raw)\n    return out[\"nonexistent\"]\n"
    );

    // act
    let output = check_helper_source(&body, &["--coverage-detail", "explain"]);

    // assert
    assert!(
        !output.stdout.contains("unknown-column"),
        "{}",
        output.stdout
    );
    assert!(
        output
            .stdout
            .contains("`run()` forwards a callable that is not a plain function name"),
        "{}",
        output.stdout
    );
}

#[test]
fn test_should_say_why_a_bare_dataframe_helper_result_has_no_columns() {
    // arrange
    let body = "from other import mystery\n\n\ndef plain(frame: pd.DataFrame) -> pd.DataFrame:\n    return mystery(frame)\n\n\ndef f(raw: pd.DataFrame):\n    out = plain(raw)\n    return out[\"x\"]\n";

    // act
    let output = check_helper_source(body, &["--coverage-detail", "explain"]);

    // assert
    assert!(
        output
            .stdout
            .contains("`plain()` is annotated to return a bare DataFrame"),
        "{}",
        output.stdout
    );
}

const POST_PROCESSING_FORWARDER: &str = "def run(name: str, fn: Callable[..., T], *args) -> T:\n    return fn(*args).assign(ts=1)\n\n\n";

#[test]
fn test_should_not_follow_a_forwarder_that_post_processes_the_result() {
    // arrange
    let body = format!(
        "def typed(frame: Annotated[pd.DataFrame, Rows]) -> Annotated[pd.DataFrame, Rows]:\n    return frame\n\n\n{POST_PROCESSING_FORWARDER}def f(raw: Annotated[pd.DataFrame, Rows]):\n    out = run(\"stage\", typed, raw)\n    return out[\"ts\"]\n"
    );

    // act
    let output = check_helper_source(&body, &[]);

    // assert
    assert!(!output.stdout.contains("-column"), "{}", output.stdout);
}

#[test]
fn test_should_not_follow_a_forwarder_that_passes_keyword_arguments() {
    // arrange
    let body = "def dropper(frame: pd.DataFrame, cols: list[str]) -> pd.DataFrame:\n    return frame.drop(columns=cols)\n\n\ndef run(name: str, fn: Callable[..., T], *args, **kwargs) -> T:\n    return fn(*args, **kwargs)\n\n\ndef f(raw: Annotated[pd.DataFrame, Rows]):\n    out = run(\"stage\", dropper, raw, cols=[\"a\"])\n    return out[\"a\"]\n";

    // act
    let output = check_helper_source(body, &[]);

    // assert
    assert!(!output.stdout.contains("-column"), "{}", output.stdout);
}

#[test]
fn test_should_follow_a_forwarder_that_returns_a_name_bound_to_the_call() {
    // arrange
    let body = format!(
        "{FORWARDER_BODY_FREE}def run(name: str, fn: Callable[..., T], *args) -> T:\n    result = fn(*args)\n    print(name)\n    return result\n\n\ndef f(raw: Annotated[pd.DataFrame, Rows]):\n    out = run(\"stage\", typed, raw)\n    return out[\"nonexistent\"]\n"
    );

    // act
    let output = check_helper_source(&body, &[]);

    // assert
    assert!(
        output.stdout.contains("error[unknown-column]"),
        "{}",
        output.stdout
    );
}

const FORWARDER_BODY_FREE: &str = "def typed(frame: Annotated[pd.DataFrame, Rows]) -> Annotated[pd.DataFrame, Rows]:\n    return frame\n\n\n";

#[test]
fn test_should_only_follow_the_type_checking_branch_of_a_type_checking_guard() {
    // arrange
    let project = Project::new();
    project.write("schema.py", ROWS_SCHEMA);
    project.write(
        "use.py",
        "from typing import TYPE_CHECKING, Annotated\n\nimport pandas as pd\n\nif TYPE_CHECKING:\n    from schema import Rows\nelse:\n    from nowhere import Rows\n\n\ndef f(raw):\n    df: Annotated[pd.DataFrame, Rows] = pd.DataFrame(raw)\n    return df[\"NoSuchColumn\"]\n",
    );

    // act
    let output = project.run(&["check", "."]);

    // assert
    assert!(
        output.stdout.contains("error[unknown-column]"),
        "{}",
        output.stdout
    );
}

#[test]
fn test_should_check_a_frame_whose_schema_is_imported_under_an_alias() {
    // arrange
    let project = Project::new();
    project.write("schema.py", ROWS_SCHEMA);
    project.write(
        "use.py",
        "from typing import Annotated\n\nimport pandas as pd\nfrom schema import Rows as Aliased\n\n\ndef f(raw):\n    df: Annotated[pd.DataFrame, Aliased] = pd.DataFrame(raw)\n    return df[\"NoSuchColumn\"]\n",
    );

    // act
    let output = project.run(&["check", "."]);

    // assert
    assert!(
        output.stdout.contains("error[unknown-column]"),
        "{}",
        output.stdout
    );
}

#[test]
fn test_should_warn_about_an_attribute_annotated_with_a_schema_that_resolves_nowhere() {
    // arrange
    let project = Project::new();
    project.write(
        "use.py",
        "from typing import Annotated\n\nimport pandas as pd\n\n\nclass Holder:\n    def __init__(self, raw):\n        self.df: Annotated[pd.DataFrame, Missing] = pd.DataFrame(raw)\n",
    );

    // act
    let output = project.run(&["check", "."]);

    // assert
    assert!(
        output.stdout.contains("warning[unresolved-schema]"),
        "{}",
        output.stdout
    );
}

#[test]
fn test_should_still_exit_1_under_strict_when_warnings_are_hidden() {
    // arrange
    let project = Project::new();
    project.write(
        "use.py",
        "from typing import Annotated\n\nimport pandas as pd\n\n\ndef f(raw):\n    df: Annotated[pd.DataFrame, Missing] = pd.DataFrame(raw)\n    return df\n",
    );

    // act
    let hidden = project.run(&["check", ".", "--no-warnings"]);
    let strict = project.run(&["check", ".", "--no-warnings", "--strict"]);

    // assert
    assert_eq!(0, hidden.code, "{}", hidden.stdout);
    assert_eq!(1, strict.code, "{}", strict.stdout);
}

#[test]
fn test_should_follow_a_schema_a_package_reexports_under_type_checking() {
    // arrange
    let project = Project::new();
    project.write("pkg/schema.py", ROWS_SCHEMA);
    project.write(
        "pkg/__init__.py",
        "from typing import TYPE_CHECKING\n\nif TYPE_CHECKING:\n    from .schema import Rows\n\n__all__ = [\"Rows\"]\n",
    );
    project.write(
        "use.py",
        "from typing import Annotated\n\nimport pandas as pd\nfrom pkg import Rows\n\n\ndef f(raw):\n    df: Annotated[pd.DataFrame, Rows] = pd.DataFrame(raw)\n    return df[\"NoSuchColumn\"]\n",
    );

    // act
    let output = project.run(&["check", "."]);

    // assert
    assert!(
        output.stdout.contains("error[unknown-column]"),
        "{}",
        output.stdout
    );
}

#[test]
fn test_should_fail_strict_but_not_a_plain_check_on_an_unverifiable_column() {
    // arrange
    let body = "def loops(frame: pd.DataFrame, names: list[str]) -> pd.DataFrame:\n    out = frame.copy()\n    for c in names:\n        out[c] = 1\n    return out\n\n\ndef f(raw: Annotated[pd.DataFrame, Rows], names: list[str]):\n    out = loops(raw, names)\n    return out[\"elsewhere\"]\n";

    // act
    let plain = check_helper_source(body, &[]);
    let strict = check_helper_source(body, &["--strict"]);

    // assert: a warning either way, and only --strict turns it into a failure
    assert!(
        plain.stdout.contains("warning[unverifiable-column]"),
        "{}",
        plain.stdout
    );
    assert_eq!(plain.code, 0, "{}", plain.stdout);
    assert_eq!(strict.code, 1, "{}", strict.stdout);
}

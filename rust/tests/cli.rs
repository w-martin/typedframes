//! End-to-end tests of the `typedframes` binary: what a user sees and the exit status they get.

use serde_json::Value;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_typedframes");

const MISSING_COLUMN: &str = r#"from typing import Annotated

import polars as pl

from typedframes import BaseSchema, Column


class UserSchema(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)


df: Annotated[pl.DataFrame, UserSchema] = pl.DataFrame({"user_id": [1], "email": ["a"]})
print(df["non_existent"])
"#;

const SAFE_SUGGESTION: &str = "import pandas as pd\n\n\ndef run():\n    df = pd.read_json(\"x.json\")\n    return df[\"a\"] + df[\"b\"]\n";

const PRIVATE_SITE: &str = "import pandas as pd\nfrom acme_internal import secret_loader\n\n\ndef run(customer_id):\n    orders = pd.read_csv(\"s3://acme/orders.csv\", sep=\";\")\n    total = orders[\"gross_margin\"].sum()\n    return clean_the_data(orders), total, customer_id\n";

struct Output {
    stdout: String,
    stderr: String,
    code: i32,
}

struct Project {
    dir: TempDir,
    cache: TempDir,
}

impl Project {
    fn new() -> Self {
        let project = Self {
            dir: tempfile::tempdir().unwrap(),
            cache: tempfile::tempdir().unwrap(),
        };
        project.write("pyproject.toml", "");
        project
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn write(&self, relative: &str, text: &str) -> PathBuf {
        let file = self.path().join(relative);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, text).unwrap();
        file
    }

    fn read(&self, relative: &str) -> String {
        fs::read_to_string(self.path().join(relative)).unwrap()
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_with_input(args, "")
    }

    fn run_with_input(&self, args: &[&str], input: &str) -> Output {
        let mut child = Command::new(BIN)
            .args(args)
            .current_dir(self.path())
            .env("TYPEDFRAMES_CACHE_DIR", self.cache.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        Output {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            code: output.status.code().unwrap_or(-1),
        }
    }
}

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

#[test]
fn test_should_keep_an_index_cache_where_told_and_clear_it_on_request() {
    // arrange
    let project = Project::new();
    project.write("a.py", "x = 1\n");
    let cache = project.path().join(".tf_cache");

    // act
    project.run(&["check", ".", "--cache-dir", cache.to_str().unwrap()]);
    let written = fs::read_dir(&cache).unwrap().count();
    let cleared = project.run(&[
        "cache",
        "clear",
        ".",
        "--cache-dir",
        cache.to_str().unwrap(),
    ]);

    // assert
    assert!(written >= 1);
    assert!(
        cleared.stdout.starts_with("Removed 1 cache file ("),
        "{}",
        cleared.stdout
    );
    assert_eq!(0, fs::read_dir(&cache).unwrap().count());
}

#[test]
fn test_should_print_a_dry_run_with_a_named_suggestion_and_change_nothing() {
    // arrange
    let project = Project::new();
    project.write("job.py", SAFE_SUGGESTION);

    // act
    let output = project.run(&["suggest", "."]);

    // assert
    assert_eq!(0, output.code);
    assert!(output.stdout.contains("1 suggestion (1 safe, 0 review), 0 withheld, in 1 file. Dry run: no files were changed."), "{}", output.stdout);
    assert!(
        output.stdout.contains("+    df = df[[\"a\", \"b\"]]"),
        "{}",
        output.stdout
    );
    assert_eq!(SAFE_SUGGESTION, project.read("job.py"));
}

#[test]
fn test_should_apply_a_safe_suggestion_when_allowed_on_a_dirty_tree() {
    // arrange
    let project = Project::new();
    project.write("job.py", SAFE_SUGGESTION);

    // act
    let output = project.run(&["suggest", ".", "--apply", "--allow-dirty"]);

    // assert
    assert_eq!(0, output.code, "{}{}", output.stdout, output.stderr);
    assert!(
        output
            .stdout
            .contains("Applied 1 suggestion in 1 file, 0 skipped."),
        "{}",
        output.stdout
    );
    assert!(
        project.read("job.py").contains(
            "    df = df[[\"a\", \"b\"]]
"
        ),
        "{}",
        project.read("job.py")
    );
}

#[test]
fn test_should_refuse_to_edit_outside_a_clean_git_work_tree() {
    // arrange
    let project = Project::new();
    project.write("job.py", SAFE_SUGGESTION);

    // act
    let output = project.run(&["suggest", ".", "--apply"]);

    // assert
    assert_eq!(1, output.code);
    assert!(output.stderr.starts_with("Error: "), "{}", output.stderr);
    assert_eq!(SAFE_SUGGESTION, project.read("job.py"));
}

#[test]
fn test_should_apply_only_the_suggestions_answered_yes_when_interactive() {
    // arrange
    let project = Project::new();
    project.write(
        "job.py",
        "import pandas as pd\n\n\ndef first():\n    df = pd.read_json(\"x.json\")\n    return df[\"a\"]\n\n\ndef second():\n    df = pd.read_json(\"y.json\")\n    return df[\"b\"]\n",
    );

    // act
    let output = project.run_with_input(
        &["suggest", ".", "--interactive", "--allow-dirty"],
        "y\nn\n",
    );

    // assert
    assert_eq!(0, output.code, "{}{}", output.stdout, output.stderr);
    let edited = project.read("job.py");
    assert!(edited.contains("df = df[[\"a\"]]"), "{edited}");
    assert!(!edited.contains("df = df[[\"b\"]]"), "{edited}");
}

#[test]
fn test_should_quit_without_changes_when_the_answer_is_q_or_input_ends() {
    // arrange
    let project = Project::new();
    project.write("job.py", SAFE_SUGGESTION);

    // act
    let quit = project.run_with_input(&["suggest", ".", "-i", "--allow-dirty"], "q\n");
    let closed = project.run_with_input(&["suggest", ".", "-i", "--allow-dirty"], "");

    // assert
    assert!(quit.stdout.contains("Quit: nothing was changed."));
    assert!(closed.stdout.contains("Quit: nothing was changed."));
    assert_eq!(SAFE_SUGGESTION, project.read("job.py"));
}

#[test]
fn test_should_refuse_a_site_name_that_matches_nothing() {
    // arrange
    let project = Project::new();
    project.write("job.py", SAFE_SUGGESTION);

    // act
    let output = project.run(&["suggest", ".", "--only", "no-such"]);

    // assert
    assert_eq!(2, output.code);
    assert!(
        output
            .stderr
            .contains("Error: no untyped site is named 'no-such'"),
        "{}",
        output.stderr
    );
}

fn first_site_name(project: &Project) -> String {
    let listing = project.run(&["suggest", ".", "--no-index"]).stdout;
    listing
        .lines()
        .find_map(|line| {
            let name = line.trim_start().split("  ").next()?;
            let (a, b) = name.split_once('-')?;
            (a.chars().all(|c| c.is_ascii_lowercase()) && b.chars().all(|c| c.is_ascii_lowercase()))
                .then(|| name.to_string())
        })
        .expect("a named site")
}

#[test]
fn test_should_report_a_site_without_the_original_names() {
    // arrange
    let project = Project::new();
    project.write("job.py", PRIVATE_SITE);
    let name = first_site_name(&project);

    // act
    let output = project.run(&["report", &name, ".", "--no-index"]);

    // assert
    assert_eq!(0, output.code, "{}", output.stderr);
    assert!(output.stdout.starts_with("<!-- Review before posting"));
    assert!(output
        .stdout
        .contains(&format!("## typedframes report: {name}")));
    assert!(output.stdout.contains("- Outcome: withheld"));
    assert!(output.stdout.contains("pd.read_csv("));
    for secret in [
        "acme",
        "customer",
        "orders",
        "gross_margin",
        "clean_the_data",
        "secret_loader",
    ] {
        assert!(
            !output.stdout.contains(secret),
            "{secret} leaked:\n{}",
            output.stdout
        );
    }
    assert!(output.stderr.contains("Read the report before posting it"));
}

#[test]
fn test_should_group_every_untyped_site_with_report_all_and_write_one_file_per_group() {
    // arrange
    let project = Project::new();
    project.write("job.py", PRIVATE_SITE);
    project.write("other.py", &PRIVATE_SITE.replace("acme", "zeta"));
    let out_dir = project.path().join("reports");

    // act
    let printed = project.run(&["report", "--all", ".", "--no-index"]);
    let written = project.run(&[
        "report",
        "--all",
        ".",
        "--no-index",
        "--output-dir",
        out_dir.to_str().unwrap(),
    ]);

    // assert
    assert!(
        printed
            .stdout
            .contains("## typedframes report: 2 untyped sites in 1 group"),
        "{}",
        printed.stdout
    );
    assert_eq!(1, fs::read_dir(&out_dir).unwrap().count());
    assert!(written.stdout.contains("01-"), "{}", written.stdout);
}

#[test]
fn test_should_say_there_are_no_untyped_sites_when_there_are_none() {
    // arrange
    let project = Project::new();
    project.write("ok.py", "x = 1\n");

    // act
    let output = project.run(&["report", "--all", "."]);

    // assert
    assert_eq!("No untyped sites.\n", output.stdout);
}

#[test]
fn test_should_exit_2_when_report_has_neither_a_name_nor_all() {
    // arrange
    let project = Project::new();

    // act
    let output = project.run(&["report", "--no-index"]);

    // assert
    assert_eq!(2, output.code);
    assert!(output
        .stderr
        .contains("Error: `report` needs a site name, or --all"));
}

use super::*;

const COLUMN_NAMES: &str = "from dataclasses import dataclass\nfrom enum import StrEnum\n\nA = \"a\"\n\n\nclass Col(StrEnum):\n    A = \"a\"\n\n\nclass Base(StrEnum):\n    pass\n\n\nclass Sub(Base):\n    A = \"a\"\n\n\n@dataclass(frozen=True)\nclass Names:\n    a: str = \"a\"\n\n\nN = Names()\n\n\n@dataclass\nclass Loose:\n    a: str = \"a\"\n\n\nL = Loose()\n";

// `pkg/app/use.py` drops the column `expr` names and reads it again.
fn column_name_project(import: &str, expr: &str, pyproject: bool) -> Project {
    let project = Project::new();
    if !pyproject {
        fs::remove_file(project.path().join("pyproject.toml")).unwrap();
    }
    project.write("pkg/__init__.py", "");
    project.write("pkg/app/__init__.py", "");
    project.write("pkg/domain/__init__.py", "from .columns import Col, N\n");
    project.write("pkg/domain/columns.py", COLUMN_NAMES);
    project.write(
        "pkg/app/use.py",
        &format!("import pandas as pd\n\n{import}\n\n\ndef f():\n    df = pd.DataFrame({{\"a\": [1], \"b\": [2]}})\n    df = df.drop(columns=[{expr}])\n    return df[{expr}]\n"),
    );
    project
}

fn column_errors(output: &Output) -> Vec<&str> {
    output
        .stdout
        .lines()
        .filter(|l| l.contains("error["))
        .collect()
}

#[test]
fn test_should_resolve_column_name_constants_from_another_module_in_every_import_form() {
    let cases = [
        ("from pkg.domain.columns import A", "A"),
        ("from pkg.domain.columns import Col", "Col.A"),
        ("from pkg.domain.columns import Col", "Col.A.value"),
        ("from pkg.domain.columns import Sub", "Sub.A"),
        ("from pkg.domain.columns import N", "N.a"),
        ("from pkg.domain.columns import Names", "Names.a"),
        ("from ..domain.columns import Col", "Col.A"),
        ("from ..domain.columns import N", "N.a"),
        ("from pkg.domain import columns as C", "C.Col.A"),
        ("from pkg.domain import columns as C", "C.N.a"),
        ("from pkg.domain import columns as C", "C.A"),
        ("import pkg.domain.columns as C", "C.Col.A"),
        ("import pkg.domain.columns as C", "C.N.a"),
        ("from pkg.domain import Col", "Col.A"),
        ("from pkg.domain import N", "N.a"),
    ];
    for (import, expr) in cases {
        for pyproject in [true, false] {
            // arrange
            let project = column_name_project(import, expr, pyproject);

            // act: a directory run and a run on the one file
            for target in [".", "pkg/app/use.py"] {
                let output = project.run(&["check", target]);

                // assert
                let errors = column_errors(&output);
                assert_eq!(
                    errors.len(),
                    1,
                    "{import} / {expr} / pyproject={pyproject} / {target}: {}",
                    output.stdout
                );
                assert!(errors[0].contains("'a'"), "{}", errors[0]);
            }
        }
    }
}

#[test]
fn test_should_name_a_missing_enum_member_reached_through_a_module() {
    for import in [
        "from pkg.domain import columns as C",
        "import pkg.domain.columns as C",
    ] {
        // arrange
        let project = column_name_project(import, "C.Col.NOPE", false);

        // act
        let output = project.run(&["check", "."]);

        // assert: the drop and the read each name the missing member
        let errors = column_errors(&output);
        assert!(!errors.is_empty(), "{import}: {}", output.stdout);
        assert!(
            errors
                .iter()
                .all(|e| e.contains("`Col` has no member 'NOPE'")),
            "{import}: {}",
            output.stdout
        );
    }
}

#[test]
fn test_should_not_resolve_a_constant_it_cannot_prove_fixed_across_modules() {
    for (import, expr) in [
        // a dataclass that is not frozen can be changed after it is built
        ("from pkg.domain.columns import L", "L.a"),
        // nothing by that name
        ("from pkg.domain.columns import Missing", "Missing.a"),
        ("from elsewhere import Col", "Col.A"),
    ] {
        // arrange
        let project = column_name_project(import, expr, true);

        // act
        let output = project.run(&["check", "."]);

        // assert
        assert!(
            column_errors(&output).is_empty(),
            "{import}: {}",
            output.stdout
        );
    }
}

#[test]
fn test_should_check_a_query_string_on_a_frame_held_on_self() {
    // arrange
    let project = Project::new();
    project.write(
        "b.py",
        "import pandas as pd\n\n\nclass B:\n    def __init__(self, f: pd.DataFrame) -> None:\n        self.frame = f.copy()\n\n    def run(self) -> None:\n        self.frame = self.frame.drop(columns=[\"z\"])\n        print(self.frame.query(\"z > 1\"))\n",
    );

    // act
    let output = project.run(&["check", "."]);

    // assert
    let errors = column_errors(&output);
    assert_eq!(errors.len(), 1, "{}", output.stdout);
    assert!(errors[0].contains("'z'"), "{}", errors[0]);
}

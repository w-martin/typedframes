use super::*;
use crate::index::build_index_internal;
use crate::ports::test_files::OsFiles;
use std::fs;

mod annotated_counts;
mod append_accumulators;
mod call_contracts;
mod call_site_requirements;
mod case_fold_and_legs;
mod constant_columns;
mod cross_file_delegates;
mod dataframe_counts;
mod derived_frames_and_arguments;
mod downstream_usage;
mod external_packages;
mod feast;
mod ignores_and_merges;
mod imports;
mod inplace_and_writes;
mod libraries;
mod literal_tracing;
mod method_resolution;
mod mutation_legs;
mod notebooks_and_messages;
mod open_frame_edits;
mod open_frames;
mod orm_models;
mod passthrough;
mod pl_col_and_narrowing;
mod rebinding;
mod receiver_libraries;
mod requirements_and_dialects;
mod returned_frames;
mod schema_parameters;
mod scoping_and_helper_edits;
mod self_attr_methods;
mod self_frames_and_queries;
mod series_dicts_and_loops;
mod sql_chains;
mod sql_tracing;
mod suggestions;
mod typed_counts;
mod typos_and_merges;
mod untyped_sites;
mod unverifiable_externals;

pub(super) fn lint_polars(body: &str) -> Vec<LintError> {
    let source =
        format!("import polars as pl\ndf = pl.read_csv(\"x.csv\", columns=[\"a\", \"b\"])\n{body}");
    Linter::new()
        .check_file_internal(&source, Path::new("test.py"))
        .unwrap()
}

pub(super) fn lint_polars_two_frames(body: &str) -> Vec<LintError> {
    let source = format!(
            "import polars as pl\ntxn = pl.read_csv(\"t.csv\", columns=[\"id\", \"amount\", \"card\"])\ncards = pl.read_csv(\"c.csv\", columns=[\"card\", \"amount\", \"limit\"])\n{body}"
        );
    Linter::new()
        .check_file_internal(&source, Path::new("test.py"))
        .unwrap()
}

// Collects the suggestions for `main_source`, in a project where `repo.py` holds
// `repo_source`.
pub(super) fn suggest_with_repo_file(repo_source: &str, main_source: &str) -> Linter {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    fs::write(root.join("repo.py"), repo_source).unwrap();
    fs::write(root.join("main.py"), main_source).unwrap();
    let index = build_index_internal(root);
    let mut linter = Linter::new();
    linter.collect_suggestions = true;
    let main_path = root.join("main.py");
    linter.load_cross_file_symbols(&index, main_source, &main_path, root);
    linter.check_file_internal(main_source, &main_path).unwrap();
    linter
}

pub(super) const PANDAS_REPO: &str = "import pandas as pd\n\n\nclass Repo:\n    def fetch(self) -> pd.DataFrame:\n        return pd.read_csv(\"x.csv\")\n";

pub(super) fn lint_for_leg_tests(body: &str) -> (Linter, Vec<LintError>) {
    let source =
        format!("import pandas as pd\ndf = pd.read_csv(\"x.csv\", usecols=[\"a\", \"b\"])\n{body}");
    let mut linter = Linter::new();
    let errors = linter
        .check_file_internal(&source, Path::new("test.py"))
        .unwrap();
    (linter, errors)
}

pub(super) fn lint_source(source: &str) -> (Linter, Vec<LintError>) {
    let mut linter = Linter::new();
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();
    (linter, errors)
}

// Indexes `helpers_source` as helpers.py and checks `main_source` as main.py against it.
pub(super) fn check_against_helpers(
    helpers_source: &str,
    main_source: &str,
) -> (Linter, Vec<LintError>) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    fs::write(root.join("helpers.py"), helpers_source).unwrap();
    fs::write(root.join("main.py"), main_source).unwrap();
    let index = build_index_internal(root);
    let mut linter = Linter::new();
    let main_path = root.join("main.py");
    linter.load_cross_file_symbols(&index, main_source, &main_path, root);
    let errors = linter.check_file_internal(main_source, &main_path).unwrap();
    (linter, errors)
}

// Indexes the given (relative path, source) files and checks `main_source` as main.py.
pub(super) fn check_in_project(
    files: &[(&str, &str)],
    main_source: &str,
) -> (Linter, Vec<LintError>) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    for (path, source) in files {
        let full = root.join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, source).unwrap();
    }
    fs::write(root.join("main.py"), main_source).unwrap();
    let index = build_index_internal(root);
    let mut linter = Linter::new();
    let main_path = root.join("main.py");
    linter.load_cross_file_symbols(&index, main_source, &main_path, root);
    let errors = linter.check_file_internal(main_source, &main_path).unwrap();
    (linter, errors)
}

pub(super) const CONFIG: &str = "BASE = [\"id\", \"amount\"]\nMETRICS = BASE + [f\"m{i}\" for i in range(2)]\n\n\ndef cols(prefix, n=1):\n    return [f\"{prefix}{i}\" for i in range(n)] + BASE\n";

pub(super) const HELPERS: &str = "def add(df):\n    df[\"z\"] = 1\n    return df\n\n\ndef clean(df):\n    return df.dropna()\n\n\ndef fix_in_place(df):\n    df[\"y\"] = 2\n\n\ndef reshape(df):\n    return df.groupby(\"a\").sum()\n";

pub(super) fn untyped_site_outcome(source: &str) -> (usize, Vec<String>) {
    let mut linter = Linter::new();
    linter.collect_suggestions = true;
    linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();
    (
        linter.suggestions.len(),
        linter.withheld.iter().map(|w| w.reason.clone()).collect(),
    )
}

pub(super) const SELF_ATTRIBUTE_COLUMNS: &str = "import pandas as pd\n\n\nclass C:\n    A = \"a\"\n\n    def f(self):\n        out = pd.DataFrame(columns=[self.A])\n        return out[\"zz\"]\n";

pub(super) fn returned_columns_message(helper_body: &str) -> String {
    let source = format!(
            "import pandas as pd\n\n\ndef make(conn, sess):\n{helper_body}\n\ndef use(conn, sess):\n    df = make(conn, sess)\n    return df[\"zz\"]\n"
        );
    let (_typed, errors) = typed_count(&source);
    assert!(
        errors.iter().all(|e| e.code != CODE_UNVERIFIABLE_COLUMN),
        "{errors:?}"
    );
    errors
        .iter()
        .find(|e| e.code == CODE_UNKNOWN_COLUMN)
        .unwrap_or_else(|| panic!("an unknown-column error: {errors:?}"))
        .message
        .clone()
}

pub(super) fn returned_columns_resolve(helper_body: &str) -> bool {
    let source = format!(
            "import pandas as pd\n\n\ndef make(conn, sess):\n{helper_body}\n\ndef use(conn, sess):\n    df = make(conn, sess)\n    return df[\"zz\"]\n"
        );
    let (_typed, errors) = typed_count(&source);
    errors.iter().any(|e| e.code == CODE_UNKNOWN_COLUMN)
}

// Lints `source` and returns (typed DataFrames, errors).
pub(super) fn typed_count(source: &str) -> (usize, Vec<LintError>) {
    let mut linter = Linter::new();
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();
    (linter.dataframes_typed, errors)
}

pub(super) fn lint(source: &str) -> (Linter, Vec<LintError>) {
    let mut linter = Linter::new();
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();
    (linter, errors)
}

// `suggest` mode on `source`, as `suggest_file` runs it.
pub(super) fn suggest_for(source: &str) -> Linter {
    let mut linter = Linter::new();
    linter.collect_suggestions = true;
    linter.suggest_display_path = "job.py".to_string();
    linter
        .check_file_internal(source, Path::new("job.py"))
        .unwrap();
    linter
}

// The reason a frame passed to `call` is withheld, for the lines of a function around it.
pub(super) fn withheld_reason_for_call(imports: &str, setup: &str, call: &str) -> String {
    let source = format!(
            "import pandas as pd\n{imports}\n\n{setup}\n\ndef run(other):\n    df = pd.read_csv(\"x.csv\")\n    print(df[\"a\"])\n    {call}\n"
        );
    let linter = suggest_for(&source);
    assert_eq!(linter.withheld.len(), 1, "{:?}", linter.suggestions);
    linter.withheld[0].reason.clone()
}

// Writes `files` (relative path, contents) under a fresh project root and suggests for
// `main`, as `test_should_count_the_columns_an_imported_helper_reads` does.
pub(super) fn suggest_in_project(files: &[(&str, &str)], main: &str) -> Linter {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    for (relative, contents) in files {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
    let main_path = root.join(main);
    let source = fs::read_to_string(&main_path).unwrap();
    let index = build_index_internal(&root);
    let mut linter = Linter::new();
    linter.collect_suggestions = true;
    linter.load_cross_file_symbols(&index, &source, &main_path, &root);
    linter.check_file_internal(&source, &main_path).unwrap();
    linter
}

pub(super) const TOTAL_HELPER: &str = "def total(frame):\n    return frame[\"a\"].sum()\n";

pub(super) const SERVICES: &str = r#"
import pandas as pd
import polars as pl


class PandasService:
    def predict(self, rows) -> pd.DataFrame:
        return pd.DataFrame(rows)


class PolarsService:
    def predict(self, rows) -> pl.DataFrame:
        return pl.DataFrame(rows)
"#;

pub(super) fn suggested_edit(linter: &Linter) -> String {
    assert_eq!(linter.suggestions.len(), 1, "{:?}", linter.withheld);
    linter.suggestions[0].edits[0].replacement.clone()
}

// Suggestions for `sut`, annotated as the pandas service, that `body` goes on to rebind.
pub(super) fn suggestions_for_a_rebound_receiver(body: &str) -> usize {
    let source = format!(
            "{SERVICES}\n\ndef run(sut: PandasService, other):\n{body}        frame = sut.predict([1])\n        return frame[\"x\"].sum()\n"
        );
    suggest_for(&source).suggestions.len()
}

pub(super) fn suggestions_for_a_service_method(class_body: &str, extra: &str) -> usize {
    let source = format!(
            "import pandas as pd\nimport polars as pl\nfrom functools import cache\n\n\nclass Service:\n{class_body}\n{extra}\n\ndef run(sut: Service):\n    frame = sut.predict([1])\n    return frame[\"x\"].sum()\n"
        );
    suggest_for(&source).suggestions.len()
}

pub(super) fn codes_of(errors: &[LintError]) -> Vec<(&str, usize)> {
    errors.iter().map(|e| (e.code.as_str(), e.line)).collect()
}

pub(super) const CLOSED: &str = "import pandas as pd\n\ndf = pd.DataFrame({\"position\": [1], \"hour\": [2], \"click\": [0]})\n";

pub(super) fn closed_frame_errors(body: &str) -> Vec<LintError> {
    lint_source(&format!("{CLOSED}{body}")).1
}

pub(super) const OPEN: &str = "import pandas as pd\n\nitems = pd.read_parquet(\"items.parquet\")\n";

pub(super) fn open_frame_errors(body: &str) -> Vec<LintError> {
    lint_source(&format!("{OPEN}{body}")).1
}

pub(super) fn without_untracked(errors: Vec<LintError>) -> Vec<LintError> {
    errors
        .into_iter()
        .filter(|e| e.code != "untracked-dataframe")
        .collect()
}

// Messages of the unknown-column errors, in line order.
pub(super) fn unknown_columns(source: &str) -> Vec<(usize, String)> {
    let (_, errors) = lint_source(source);
    errors
        .into_iter()
        .filter(|e| e.code == CODE_UNKNOWN_COLUMN)
        .map(|e| (e.line, e.message))
        .collect()
}

pub(super) const FRAME: &str =
    "import pandas as pd\n\nf = pd.DataFrame({\"a\": [1], \"b\": [2]})\n";

pub(super) fn lint_polars_source(body: &str) -> (Linter, Vec<LintError>) {
    lint_source(&format!(
        "import polars as pl\ndf = pl.read_csv(\"x.csv\", columns=[\"a\", \"b\"])\n{body}"
    ))
}

pub(super) fn unknown_column_errors(errors: &[LintError]) -> Vec<(usize, String)> {
    errors
        .iter()
        .filter(|e| e.code == CODE_UNKNOWN_COLUMN)
        .map(|e| (e.line, e.message.clone()))
        .collect()
}

pub(super) fn errors_for(source: &str) -> Vec<LintError> {
    Linter::new()
        .check_file_internal(source, Path::new("test.py"))
        .unwrap()
}

pub(super) const TWO_FRAMES: &str = "import pandas as pd\n\nx = pd.read_csv(\"x.csv\", usecols=[\"a\"])\ny = pd.read_csv(\"y.csv\", usecols=[\"c\"])\n";

pub(super) const COMBINE: &str =
    "def combine(left, right):\n    return left[\"a\"] + right[\"b\"]\n";

pub(super) fn missing_columns(errors: &[LintError]) -> Vec<&LintError> {
    errors
        .iter()
        .filter(|e| e.code == CODE_MISSING_COLUMN)
        .collect()
}

pub(super) const PROTOCOL_MAIN: &str = "import pandas as pd\nfrom typing import Protocol\n\nfrom steps import A, B\n\n\nclass Step(Protocol):\n    def apply(self, left, right): ...\n\n\ndef run(step: Step):\n    x = pd.read_csv(\"x.csv\", usecols=[\"a\"])\n    return step.apply(x, x)\n";

pub(super) const GUARDED_MAYBE: &str =
    "def maybe(df):\n    if \"a\" in df.columns:\n        return df[\"a\"]\n    return None\n";

pub(super) fn unknown_names(source: &str) -> Vec<String> {
    let (_, errors) = lint_source(source);
    errors
        .iter()
        .filter(|e| e.code == CODE_UNKNOWN_COLUMN)
        .map(|e| e.message.split('\'').nth(1).unwrap_or_default().to_string())
        .collect()
}

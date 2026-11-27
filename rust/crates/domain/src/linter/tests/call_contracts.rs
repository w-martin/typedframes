use super::*;

#[test]
fn test_should_check_a_frame_passed_by_keyword() {
    // arrange
    let source = format!("{TWO_FRAMES}\n\n{COMBINE}\n\ncombine(x, right=y)\n");

    // act
    let errors = errors_for(&source);

    // assert
    let missing = missing_columns(&errors);
    assert_eq!(missing.len(), 1, "errors: {errors:?}");
    assert!(missing[0].message.contains("'y' passed to combine"));
    assert!(missing[0].message.contains("as 'right'"));
    assert!(missing[0].message.contains("{b}"));
}

#[test]
fn test_should_check_a_frame_passed_as_the_second_positional_argument() {
    // arrange
    let source = format!("{TWO_FRAMES}\n\n{COMBINE}\n\nout = combine(x, y)\n");

    // act
    let errors = errors_for(&source);

    // assert
    let missing = missing_columns(&errors);
    assert_eq!(missing.len(), 1, "errors: {errors:?}");
    assert!(missing[0].message.contains("'y' passed to combine"));
}

#[test]
fn test_should_map_keywords_to_their_parameters_in_any_order() {
    // arrange: each frame lacks what its own parameter reads.
    let source = format!("{TWO_FRAMES}\n\n{COMBINE}\n\ncombine(right=x, left=y)\n");

    // act
    let errors = errors_for(&source);

    // assert
    let missing = missing_columns(&errors);
    assert_eq!(missing.len(), 2, "errors: {errors:?}");
}

#[test]
fn test_should_not_check_a_call_whose_arguments_cannot_be_bound() {
    // arrange: unpacked arguments, and an unknown keyword.
    let source = format!(
        "{TWO_FRAMES}\n\n{COMBINE}\n\nargs = [x, y]\ncombine(*args)\ncombine(x, y, extra=1)\n"
    );

    // act
    let errors = errors_for(&source);

    // assert
    assert!(missing_columns(&errors).is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_check_a_call_to_a_method_of_the_enclosing_class() {
    // arrange: the method is defined after the one calling it.
    let source = "import pandas as pd\n\n\nclass Pipeline:\n    def run(self):\n        x = pd.read_csv(\"x.csv\", usecols=[\"a\"])\n        return self.join(x, x)\n\n    def join(self, left, right):\n        return left[\"a\"] + right[\"z\"]\n";

    // act
    let errors = errors_for(source);

    // assert
    let missing = missing_columns(&errors);
    assert_eq!(missing.len(), 1, "errors: {errors:?}");
    assert!(missing[0].message.contains("as 'right'"));
}

#[test]
fn test_should_not_check_a_self_call_a_subclass_overrides_differently() {
    // arrange
    let source = "import pandas as pd\n\n\nclass Pipeline:\n    def run(self):\n        x = pd.read_csv(\"x.csv\", usecols=[\"a\"])\n        return self.join(x, x)\n\n    def join(self, left, right):\n        return right[\"z\"]\n\n\nclass Other(Pipeline):\n    def join(self, left, right):\n        return right[\"a\"]\n";

    // act
    let errors = errors_for(source);

    // assert
    assert!(missing_columns(&errors).is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_check_a_helper_imported_from_another_file() {
    // arrange
    let main = format!("from helpers import combine\nimport helpers\n{TWO_FRAMES}\ncombine(x, y)\nhelpers.combine(left=x, right=y)\n");

    // act
    let (_, errors) = check_against_helpers(COMBINE, &main);

    // assert
    let missing = missing_columns(&errors);
    assert_eq!(missing.len(), 2, "errors: {errors:?}");
    assert!(missing.iter().all(|e| e.message.contains("helpers.py")));
}

#[test]
fn test_should_stay_silent_when_a_name_binds_to_two_same_named_functions() {
    // arrange: `combine` is imported from two files, and `tool.combine` names no module.
    let other = "def combine(left, right):\n    return right[\"zz\"]\n";
    let main = format!("from helpers import combine\nfrom other import combine\n{TWO_FRAMES}\ncombine(x, y)\ntool = make_tool()\ntool.combine(x, y)\n");

    // act
    let (_, errors) = check_in_project(&[("helpers.py", COMBINE), ("other.py", other)], &main);

    // assert
    assert!(missing_columns(&errors).is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_not_check_a_self_call_a_subclass_in_another_file_overrides() {
    // arrange
    let main = "import pandas as pd\n\n\nclass Pipeline:\n    def go(self):\n        x = pd.read_csv(\"x.csv\", usecols=[\"a\"])\n        return self.join(x, x)\n\n    def join(self, left, right):\n        return right[\"z\"]\n";
    let sub = "from main import Pipeline\n\n\nclass Fast(Pipeline):\n    def join(self, left, right):\n        return left\n";

    // act
    let (_, errors) = check_in_project(&[("sub.py", sub)], main);

    // assert
    assert!(missing_columns(&errors).is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_stay_silent_on_a_protocol_whose_implementors_disagree() {
    // arrange
    let steps = "class A:\n    def apply(self, left, right):\n        return right[\"b\"]\n\n\nclass B:\n    def apply(self, left, right):\n        return right[\"c\"]\n";

    // act
    let (_, errors) = check_in_project(&[("steps.py", steps)], PROTOCOL_MAIN);

    // assert
    assert!(missing_columns(&errors).is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_check_a_protocol_call_every_implementor_agrees_on() {
    // arrange
    let steps = "class A:\n    def apply(self, left, right):\n        return right[\"b\"]\n\n\nclass B:\n    def apply(self, left, right):\n        out = right[\"b\"] * 2\n        return out\n";

    // act
    let (_, errors) = check_in_project(&[("steps.py", steps)], PROTOCOL_MAIN);

    // assert
    let missing = missing_columns(&errors);
    assert_eq!(missing.len(), 1, "errors: {errors:?}");
    assert!(missing[0].message.contains("every 'apply'"));
}

#[test]
fn test_should_inherit_the_requirement_of_a_helper_a_parameter_is_forwarded_to() {
    // arrange
    let source = format!("{TWO_FRAMES}\n\n{COMBINE}\n\ndef outer(first, second):\n    return combine(first, right=second)\n\n\nouter(x, y)\n");

    // act
    let errors = errors_for(&source);

    // assert
    let missing = missing_columns(&errors);
    assert_eq!(missing.len(), 1, "errors: {errors:?}");
    assert!(missing[0].message.contains("'y' passed to outer"));
}

#[test]
fn test_should_inherit_a_forwarded_requirement_across_files() {
    // arrange: middle.py forwards to base.py; main.py only sees middle.py.
    let middle = "from helpers import combine\n\n\ndef outer(first, second):\n    return combine(first, second)\n";
    let main = format!("from middle import outer\n{TWO_FRAMES}\nouter(x, y)\n");

    // act
    let (_, errors) = check_in_project(&[("helpers.py", COMBINE), ("middle.py", middle)], &main);

    // assert
    let missing = missing_columns(&errors);
    assert_eq!(missing.len(), 1, "errors: {errors:?}");
    assert!(missing[0].message.contains("'y' passed to outer"));
}

#[test]
fn test_should_not_fold_a_constant_another_file_changes() {
    // arrange
    let cfg = "COLS = [\"a\"]\nNAME = \"a\"\nKEEP = [\"a\"]\n";
    let plugin = "import cfg\nfrom cfg import COLS\n\ncfg.NAME = \"b\"\nCOLS.append(\"b\")\n";
    let main = "import pandas as pd\n\nimport plugin  # noqa: F401\nfrom cfg import COLS, KEEP, NAME\n\na = pd.read_csv(\"x.csv\", usecols=COLS)\nb = pd.DataFrame({NAME: [1]})\nc = pd.read_csv(\"x.csv\", usecols=KEEP)\nprint(a[\"b\"], b[\"b\"], c[\"b\"])\n";

    // act
    let (_, errors) = check_in_project(&[("cfg.py", cfg), ("plugin.py", plugin)], main);

    // assert
    let unknown: Vec<&LintError> = errors
        .iter()
        .filter(|e| e.code == CODE_UNKNOWN_COLUMN)
        .collect();
    assert_eq!(unknown.len(), 1, "{errors:?}");
    assert!(unknown[0].message.contains("line 8"), "{errors:?}");
}

#[test]
fn test_should_not_require_a_column_a_helper_reads_only_behind_a_guard() {
    // arrange
    let source = format!("import pandas as pd\n\n\n{GUARDED_MAYBE}\n\nclass K:\n    def maybe(self, df):\n        if \"a\" in df.columns:\n            return df[\"a\"]\n        return None\n\n    def run(self):\n        df = pd.DataFrame({{\"b\": [1]}})\n        return self.maybe(df)\n\n\ndef h():\n    df = pd.read_csv(\"x.csv\", usecols=[\"b\"])\n    return maybe(df)\n");

    // act
    let errors = errors_for(&source);

    // assert
    assert!(missing_columns(&errors).is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_not_require_a_guarded_read_of_an_imported_helper() {
    // arrange
    let main = "import pandas as pd\n\nimport helpers\nfrom helpers import maybe\n\n\ndef g():\n    df = pd.DataFrame({\"b\": [1]})\n    return helpers.maybe(df)\n\n\ndef h():\n    df = pd.DataFrame({\"b\": [1]})\n    return maybe(df)\n";

    // act
    let (_, errors) = check_in_project(&[("helpers.py", GUARDED_MAYBE)], main);

    // assert
    assert!(missing_columns(&errors).is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_not_resolve_a_method_an_instance_attribute_shadows() {
    // arrange
    let source = "import pandas as pd\n\n\nclass Base:\n    def process(self, df):\n        return df[\"a\"]\n\n\nclass D(Base):\n    def __init__(self):\n        self.process = lambda df: df[\"b\"]\n\n    def run(self):\n        df = pd.DataFrame({\"b\": [1]})\n        return self.process(df)\n\n\nclass E(Base):\n    def run(self):\n        df = pd.DataFrame({\"b\": [1]})\n        return self.process(df)\n";

    // act
    let errors = errors_for(source);

    // assert
    let missing = missing_columns(&errors);
    assert_eq!(missing.len(), 1, "errors: {errors:?}");
    assert_eq!(missing[0].line, 21);
}

#[test]
fn test_should_not_check_a_frame_whose_columns_are_unknown() {
    // arrange
    let source = format!("import pandas as pd\n\n{COMBINE}\n\nx = pd.read_csv(\"x.csv\", usecols=[\"a\"])\ny = pd.read_csv(\"y.csv\")\ncombine(x, y)\n\n\ndef job(frame):\n    return combine(x, frame)\n");

    // act
    let errors = errors_for(&source);

    // assert
    assert!(missing_columns(&errors).is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_report_nothing_for_a_helper_no_one_calls() {
    // arrange
    let source = format!("import pandas as pd\n\n{COMBINE}\n\ndef outer(first, second):\n    return combine(first, second)\n");

    // act
    let errors = errors_for(&source);

    // assert
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_accept_frames_with_every_required_column() {
    // arrange
    let source = format!("import pandas as pd\n\n{COMBINE}\n\nx = pd.read_csv(\"x.csv\", usecols=[\"a\"])\ny = pd.read_csv(\"y.csv\", usecols=[\"b\", \"c\"])\ncombine(x, y)\ncombine(right=y, left=x)\n");

    // act
    let errors = errors_for(&source);

    // assert
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_not_require_a_column_read_only_after_a_branch_or_a_write() {
    // arrange
    let source = format!("{TWO_FRAMES}\n\ndef f(left, right):\n    right[\"b\"] = 1\n    out = right[\"b\"]\n    if \"d\" in right.columns:\n        return right[\"d\"]\n    return right[\"e\"]\n\n\nf(x, y)\n");

    // act
    let errors = errors_for(&source);

    // assert
    assert!(missing_columns(&errors).is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_withhold_usecols_when_a_renamed_away_name_is_read() {
    // arrange
    let source = "import pandas as pd\nst = pd.read_csv(\"s.csv\")\nst.rename(columns={\"dock_type\": \"dock\"}, inplace=True)\nprint(st[[\"station_id\", \"dock_type\"]])\n";

    // act
    let linter = suggest_for(source);

    // assert
    assert!(linter.suggestions.is_empty());
    assert!(linter.withheld[0]
        .reason
        .contains("reads 'dock_type' after a rename or drop removed it"));
}

#[test]
fn test_should_report_a_list_selection_of_a_renamed_away_column_on_an_open_frame() {
    // arrange
    let source = "import pandas as pd\nst = pd.read_csv(\"s.csv\")\nst.rename(columns={\"dock_type\": \"dock\"}, inplace=True)\nprint(st[[\"station_id\", \"dock_type\"]])\n";

    // act
    let errors = errors_for(source);

    // assert
    assert!(errors
        .iter()
        .any(|e| e.line == 4 && e.message.contains("renamed to 'dock'")));
}

#[test]
fn test_should_report_a_dropped_column_read_off_a_frame_stored_on_self() {
    // arrange
    let source = "import pandas as pd\n\nclass Pipe:\n    def __init__(self) -> None:\n        self.df = pd.DataFrame({\"sid\": [1], \"dock\": [\"a\"]})\n\n    def encode(self):\n        self.df = self.df.drop(columns=[\"dock\"])\n        return self.df[\"dock\"]\n";

    // act
    let errors = errors_for(source);

    // assert
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0]
        .message
        .contains("`self.df`: it was dropped at line 8"));
}

#[test]
fn test_should_report_a_typo_on_an_aggregate_stored_on_self() {
    // arrange
    let source = "import pandas as pd\n\nclass Pipe:\n    def __init__(self, maint: pd.DataFrame) -> None:\n        self.maint = maint\n\n    def summarise(self):\n        self.ev = self.maint.groupby(\"sid\").agg(total_cost=(\"cost\", \"sum\")).reset_index()\n        return self.ev[\"total_cst\"]\n";

    // act
    let errors = errors_for(source);

    // assert
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].message.contains("did you mean 'total_cost'"));
}

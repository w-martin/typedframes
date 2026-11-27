use super::*;

#[test]
fn test_should_forget_self_frame_facts_after_a_call_that_reaches_self() {
    // arrange
    let source = "import pandas as pd\n\nclass Pipe:\n    def __init__(self) -> None:\n        self.df = pd.DataFrame({\"a\": [1], \"b\": [2]})\n\n    def restore(self) -> None:\n        self.df[\"b\"] = 1\n\n    def run(self):\n        self.df = self.df.drop(columns=[\"b\"])\n        self.restore()\n        return self.df[\"b\"]\n";

    // act
    let errors = errors_for(source);

    // assert
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn test_should_forget_self_frame_facts_after_calls_that_reach_self_without_naming_it() {
    // arrange
    let source = r#"import pandas as pd


class Child:
    def __init__(self, parent):
        self.parent = parent

    def fill(self):
        self.parent.df = self.parent.df.assign(b=1)


class Base:
    def enrich(self):
        self.df = self.df.assign(b=1)


class P(Base):
    def __init__(self):
        self.df = pd.DataFrame({"a": [1]})
        self.child = Child(self)

    def via_child(self):
        self.df = pd.DataFrame({"a": [1]})
        c = self.child
        c.fill()
        return self.df["b"]

    def via_alias(self):
        self.df = pd.DataFrame({"a": [1]})
        obj = self
        obj.df = obj.df.assign(b=2)
        return self.df["b"]

    def via_super(self):
        self.df = pd.DataFrame({"a": [1]})
        super().enrich()
        return self.df["b"]

    def via_closure(self):
        self.df = pd.DataFrame({"a": [1]})

        def add():
            self.df = self.df.assign(b=1)

        add()
        return self.df["b"]

    def via_hooks(self, hooks):
        self.df = pd.DataFrame({"a": [1]})
        for hook in hooks:
            hook()
        return self.df["b"]
"#;

    // act
    let errors = errors_for(source);

    // assert
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn test_should_not_take_row_drops_level_drops_or_reset_index_as_lost_columns() {
    // arrange
    let source = r#"import pandas as pd


def rows(path):
    df = pd.read_parquet(path)
    df = df.drop("x")
    return df["x"]


def rows_in_place(path):
    df = pd.read_parquet(path)
    df.drop(["x"], inplace=True)
    return df["x"]


def known_rows():
    df = pd.DataFrame({"x": [1, 2]}, index=["x", "y"])
    df = df.drop("x")
    return df["x"]


def level(path):
    df = pd.read_parquet(path)
    df = df.drop(columns="x", level=1)
    return df["x"]


def reset(path):
    df = pd.read_parquet(path)
    df = df.drop(columns=["x"])
    df = df.reset_index()
    return df["x"]


def dummies(path):
    df = pd.read_parquet(path)
    df = pd.get_dummies(df, columns=["x"], prefix="", prefix_sep="")
    return df["x"]


def still_gone(path):
    df = pd.read_parquet(path)
    df = df.drop(columns=["x"])
    df = df.reset_index(drop=True)
    return df["x"]
"#;

    // act
    let errors = errors_for(source);

    // assert
    let unknown: Vec<usize> = errors
        .iter()
        .filter(|e| e.code == CODE_UNKNOWN_COLUMN)
        .map(|e| e.line)
        .collect();
    assert_eq!(unknown, [45], "{errors:?}");
}

#[test]
fn test_should_take_positional_drop_labels_as_columns_on_a_polars_frame() {
    // arrange
    let source = "import polars as pl\n\n\ndef f(path):\n    df = pl.read_parquet(path)\n    df = df.drop(\"x\")\n    return df[\"x\"]\n";

    // act
    let errors = errors_for(source);

    // assert
    assert!(
        errors
            .iter()
            .any(|e| e.line == 7 && e.code == CODE_UNKNOWN_COLUMN),
        "{errors:?}"
    );
}

#[test]
fn test_should_keep_self_frame_facts_across_frame_methods_and_builtins() {
    // arrange
    let source = r#"import pandas as pd


class P:
    def run(self):
        self.df = pd.DataFrame({"a": [1]})
        print(len(self.df), self.df.shape)
        self.df = self.df.dropna()
        return self.df["b"]
"#;

    // act
    let errors = errors_for(source);

    // assert
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].line, 9);
}

#[test]
fn test_should_not_suggest_usecols_for_a_frame_stored_on_self() {
    // arrange
    let source = "import pandas as pd\n\nclass Loader:\n    def __init__(self, path: str) -> None:\n        self.df = pd.read_csv(path)\n        self.n = len(self.df[\"a\"])\n\n    def total(self):\n        return self.df[\"b\"].sum()\n";

    // act
    let linter = suggest_for(source);

    // assert
    assert!(linter.suggestions.is_empty());
    assert_eq!(linter.withheld.len(), 1);
}

#[test]
fn test_should_check_names_in_query_and_eval_strings() {
    // arrange
    let source =
        "import pandas as pd\n\ndf = pd.read_csv(\"f.csv\", usecols=[\"a\", \"b\"])\nlim = 3\n\
            x = df.query(\"a > @lim and typo > 3 and b.str.contains('zz') and `b` > 1\")\n\
            y = df.eval(\"a + typo2\")\nw = df.eval(\"c = a + b\")\nprint(w[\"c\"], w[\"nope\"])\n";

    // act
    let unknown = unknown_names(source);

    // assert
    assert_eq!(unknown, ["typo", "typo2", "nope"]);
}

#[test]
fn test_should_not_report_a_name_a_constructor_or_assignment_gave_the_index() {
    // arrange
    let source = r#"import pandas as pd


def named_index():
    df = pd.DataFrame({"v": [2]}, index=pd.Index([1], name="date"))
    df2 = df.copy()
    return df.query("date > 0"), df2.sort_values("date"), df.reset_index()["date"]


def unknown_name(idx):
    df = pd.DataFrame({"v": [2]}, index=idx)
    return df.groupby("date").sum()


def set_name():
    df = pd.DataFrame({"v": [2]})
    df.index.name = "date"
    return df.groupby("date").sum()


def in_place():
    df = pd.DataFrame({"v": [2]})
    df.rename_axis("date", inplace=True)
    return df.sort_values("date")


def unnamed():
    df = pd.DataFrame({"v": [2]}, index=[1])
    return df.sort_values("date")
"#;

    // act
    let unknown = unknown_names(source);

    // assert
    assert_eq!(unknown, ["date"]);
}

#[test]
fn test_should_not_check_query_names_that_may_be_index_levels_or_resolvers() {
    // arrange
    let source = "import pandas as pd\n\ndf = pd.read_csv(\"f.csv\", usecols=[\"a\", \"b\"])\n\
            s = df.set_index(\"a\")\nprint(s.query(\"a > 1\"), df.set_index(\"a\").query(\"a > 1\"))\n\
            print(df.query(\"zz > 1\", local_dict={\"zz\": 1}), df.query(\"index > 2\"))\n";

    // act / assert
    assert!(unknown_names(source).is_empty());
}

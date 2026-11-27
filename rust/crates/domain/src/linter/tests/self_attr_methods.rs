use super::*;

#[test]
fn test_should_resolve_self_attr_method_call_to_a_named_schema() {
    // arrange: `self._data_repository.get(...)` -- a method call on an instance
    // attribute assigned in __init__, not a bare name or module.func() call. This
    // is the reported real-world shape (a repository/wrapper class) that was
    // previously invisible to the checker entirely: the call's receiver
    // (self._data_repository) is a nested Attribute, not a Name, so it never
    // reached any recognized pattern.
    let source = r#"
from typing import Annotated
import pandas as pd
from typedframes import BaseSchema, Column


class TrainingSchema(BaseSchema):
    feature_a = Column(type=float)
    label = Column(type=int)


class DataRepository:
    def get_training(self) -> Annotated[pd.DataFrame, TrainingSchema]:
        return pd.read_csv("training.csv")


class Pipeline:
    def __init__(self):
        self._data_repository = DataRepository()

    def run(self):
        df = self._data_repository.get_training()
        print(df["feature_a"])
        print(df["nonexistent"])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:#?}");
    assert!(errors[0].message.contains("nonexistent"));
    assert!(errors[0].message.contains("TrainingSchema"));
    assert_eq!(linter.dataframes_total, 1);
    assert_eq!(linter.dataframes_typed, 1);
}

#[test]
fn test_should_report_unverifiable_for_bare_dataframe_return_via_self_attr() {
    // arrange: DataRepository.get_raw returns a bare pd.DataFrame, no Schema --
    // recognized (dataframes_total) but not typed, and column access unverifiable.
    let source = r#"
import pandas as pd


class DataRepository:
    def get_raw(self) -> pd.DataFrame:
        return pd.read_csv("raw.csv")


class Pipeline:
    def __init__(self):
        self._data_repository = DataRepository()

    def run(self):
        raw = self._data_repository.get_raw()
        print(raw["anything_at_all"])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    let unverifiable = errors
        .iter()
        .filter(|e| e.code == CODE_UNVERIFIABLE_COLUMN)
        .count();
    assert_eq!(unverifiable, 1, "errors: {errors:#?}");
    assert_eq!(linter.dataframes_typed, 0);
}

#[test]
fn test_should_resolve_self_attr_method_with_no_return_annotation_via_body_inference() {
    // arrange: the exact real-world shape reported -- a repository class method
    // with NO return annotation at all, whose real schema is only inferable from
    // its body (mirrors test_should_count_typed_dataframe_for_load_call_with_usecols'
    // body-inference for module-level functions, applied through self.<attr>).
    let source = r#"
import pandas as pd
from typedframes import BaseSchema, Column


class OrderSchema(BaseSchema):
    order_id = Column(type=int)
    total = Column(type=float)


class Warehouse:
    def fetch_orders(self):
        df = pd.read_csv("orders.csv", usecols=["order_id", "total"])
        return df


class Reporter:
    def __init__(self):
        self._warehouse = Warehouse()

    def summarize(self):
        orders = self._warehouse.fetch_orders()
        print(orders["order_id"])
        print(orders["not_a_real_column"])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert: 1 DataFrame origin, not 2 -- fetch_orders is a pure pass-through
    // (`df = pd.read_csv(...); return df`, nothing modifies `df` before the
    // return), so the inner read_csv's count moves to the outer
    // `self._warehouse.fetch_orders()` call site this test is actually about,
    // rather than being counted once inside fetch_orders' own body AND again at
    // the call site (see the journey/leg model: one DataFrame per execution-path
    // instance).
    assert_eq!(errors.len(), 1, "errors: {errors:#?}");
    assert!(errors[0].message.contains("not_a_real_column"));
    assert_eq!(linter.dataframes_total, 1);
    assert_eq!(linter.dataframes_typed, 1);
}

#[test]
fn test_should_not_confuse_same_named_methods_on_different_classes_via_self_attr() {
    // arrange: guards against the exact false-positive shortcut that was
    // deliberately ruled out (just checking a flat method-name map regardless of
    // which class it belongs to). Two classes both define `get_training`, with
    // DIFFERENT return schemas -- resolution must pick DataRepository's, not
    // UnrelatedThing's, because current_self_attrs says _data_repository IS a
    // DataRepository.
    let source = r#"
from typing import Annotated
import pandas as pd
from typedframes import BaseSchema, Column


class TrainingSchema(BaseSchema):
    feature_a = Column(type=float)


class DecoySchema(BaseSchema):
    totally_different_col = Column(type=str)


class DataRepository:
    def get_training(self) -> Annotated[pd.DataFrame, TrainingSchema]:
        return pd.read_csv("training.csv")


class UnrelatedThing:
    def get_training(self) -> Annotated[pd.DataFrame, DecoySchema]:
        return pd.read_csv("decoy.csv")


class Pipeline:
    def __init__(self):
        self._data_repository = DataRepository()

    def run(self):
        df = self._data_repository.get_training()
        print(df["feature_a"])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert: feature_a is valid on TrainingSchema (the real receiver type) --
    // if resolution had collided with UnrelatedThing.get_training's DecoySchema
    // instead, this would incorrectly report feature_a as unknown.
    assert!(errors.is_empty(), "errors: {errors:#?}");
}

#[test]
fn test_should_resolve_dataframe_returned_through_a_protocol_typed_call() {
    // arrange: SOLID/DI-style structural typing -- `process` takes a `DataSource`
    // Protocol (an interface, never instantiated) and calls its `load()` method; the
    // concrete return schema lives on `SqlDataSource`, an unrelated-by-inheritance
    // class. The parameter's annotation names the interface, whose own `load` is a bare
    // `-> pd.DataFrame`; the one implementation in the file settles the columns.
    let source = r#"
from typing import Protocol
import pandas as pd

class DataSource(Protocol):
    def load(self) -> pd.DataFrame: ...

class SqlDataSource:
    def load(self) -> pd.DataFrame:
        df = pd.read_csv("data.csv", usecols=["a", "b"])
        return df

def process(source: DataSource) -> None:
    df = source.load()
    print(df["a"])
    print(df["c"])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert: 'c' isn't in the {a, b} schema inferred from SqlDataSource.load's
    // body, and must be caught at the Protocol-typed call site.
    assert_eq!(errors.len(), 1, "expected one error, got: {errors:?}");
    assert_eq!(errors[0].code, "unknown-column");
    assert!(errors[0].message.contains("'c'"));
    assert!(errors[0].message.contains("{a, b}"));
}

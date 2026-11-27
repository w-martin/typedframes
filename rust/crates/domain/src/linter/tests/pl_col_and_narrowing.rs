use super::*;

#[test]
fn test_should_validate_pl_col_in_select() {
    // arrange
    let source = r#"
from typing import Annotated
from typedframes import BaseSchema, Column
import polars as pl

class OrderSchema(BaseSchema):
    order_id = Column(type=int)
    amount = Column(type=float)

df: Annotated[pl.DataFrame, OrderSchema] = pl.read_csv("orders.csv")
result = df.select(pl.col("amount"))
bad = df.select(pl.col("revenue"))
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(errors.len(), 1);
    assert!(errors[0].message.contains("revenue"));
    assert!(errors[0].message.contains("OrderSchema"));
}

#[test]
fn test_should_validate_pl_col_in_filter() {
    // arrange
    let source = r#"
from typing import Annotated
from typedframes import BaseSchema, Column
import polars as pl

class UserSchema(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)

df: Annotated[pl.DataFrame, UserSchema] = pl.read_csv("users.csv")
result = df.filter(pl.col("user_id") > 10)
bad = df.filter(pl.col("username") == "alice")
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(errors.len(), 1);
    assert!(errors[0].message.contains("username"));
}

#[test]
fn test_should_validate_pl_col_list_in_select() {
    // arrange
    let source = r#"
from typing import Annotated
from typedframes import BaseSchema, Column
import polars as pl

class SalesSchema(BaseSchema):
    region = Column(type=str)
    revenue = Column(type=float)

df: Annotated[pl.DataFrame, SalesSchema] = pl.read_csv("sales.csv")
result = df.select([pl.col("region"), pl.col("revenue")])
bad = df.select([pl.col("region"), pl.col("profit")])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(errors.len(), 1);
    assert!(errors[0].message.contains("profit"));
}

#[test]
fn test_should_validate_bare_col_import() {
    // arrange
    let source = r#"
from typing import Annotated
from typedframes import BaseSchema, Column
import polars as pl
from polars import col

class ItemSchema(BaseSchema):
    item_id = Column(type=int)
    price = Column(type=float)

df: Annotated[pl.DataFrame, ItemSchema] = None
result = df.select(col("price"))
bad = df.select(col("cost"))
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(errors.len(), 1);
    assert!(errors[0].message.contains("cost"));
}

#[test]
fn test_should_narrow_to_an_exact_schema_through_loc_column_selection() {
    // arrange: `.loc[:, [...]]` -- the row part is irrelevant to the column set.
    let (_, errors) =
        lint_for_leg_tests("out = df.loc[:, [\"a\"]]\nprint(out[\"a\"])\nprint(out[\"b\"])\n");

    // assert: `b` was dropped by the selection, correctly flagged against {a}, not
    // silently allowed through a preserved {a, b}.
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'b'"));
    assert!(errors[0].message.contains("{a}"));
}

#[test]
fn test_should_narrow_to_an_exact_schema_through_loc_on_an_untracked_base() {
    // arrange: `.loc[:, [...]]` on a base the checker never resolved a schema for
    // still narrows to an exact, typed one -- same principle as `df[["a", "b"]]`.
    let (_linter, errors) = lint_source(
            "import pandas as pd\n\n\ndef load() -> pd.DataFrame:\n    ...\n\n\ndf = load()\nout = df.loc[:, [\"a\", \"b\"]]\nprint(out[\"a\"])\nprint(out[\"nope\"])\n",
        );

    // assert: same "narrowing, not a new counted origin" accounting as `[[...]]`
    // above -- what matters here is that `nope` is correctly caught at all.
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'nope'"));
}

#[test]
fn test_should_keep_the_columns_of_a_single_indexer_loc_row_mask_under_a_new_name() {
    // arrange: `.loc[mask]` (one indexer, a row mask) keeps every column, so a new
    // name bound from it is checked against the base frame's columns.
    let (linter, errors) =
        lint_for_leg_tests("mask = df[\"a\"] > 1\nout = df.loc[mask]\nprint(out[\"zzz\"])\n");

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'zzz'"));
    assert!(linter.variables.contains_key("out"));
}

#[test]
fn test_should_narrow_to_an_exact_schema_through_filter_items() {
    // arrange: pandas' `.filter(items=[...])` selects COLUMNS -- this used to be
    // wrongly treated as a row-preserving op (same method name, different meaning
    // from polars/pandas' own row-filtering `.filter(mask)`), silently keeping the
    // full base schema and missing a real typo on an excluded column.
    let (_, errors) = lint_for_leg_tests("out = df.filter(items=[\"a\"])\nprint(out[\"b\"])\n");

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'b'"));
    assert!(errors[0].message.contains("{a}"));
}

#[test]
fn test_should_leave_filter_items_alone_when_axis_selects_the_index() {
    // arrange: `axis=0` redirects `items=` to INDEX LABELS, not columns --
    // regression test for a bug where this was still treated as a column
    // selection regardless of axis=, producing a false positive on the bogus
    // "column" AND corrupting the schema so a genuinely valid column access
    // failed too.
    let (_, errors) =
        lint_for_leg_tests("out = df.filter(items=[\"row1\"], axis=0)\nprint(out[\"a\"])\n");

    // assert: row-preserving, same as `.filter(mask)` -- base schema {a, b}
    // unchanged, zero errors.
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_leave_a_positional_filter_mask_as_row_preserving() {
    // arrange: no items=/like=/regex= kwarg -- the ROW-filtering shape, unaffected.
    let (_, errors) = lint_for_leg_tests("out = df.filter(df[\"a\"] > 1)\nprint(out[\"zzz\"])\n");

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("{a, b}"));
}

#[test]
fn test_should_mark_unresolved_after_filter_items_with_a_dynamic_list() {
    // arrange: still a column selection, just not one whose result is knowable --
    // must not be reported as a false unknown-column against the stale full schema.
    let (_, errors) =
        lint_for_leg_tests("cols = get_cols()\nout = df.filter(items=cols)\nprint(out[\"zzz\"])\n");

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
}

#[test]
fn test_should_mark_unresolved_after_filter_like() {
    // arrange: `like=`/`regex=` are inherently dynamic pattern selections.
    let (_, errors) =
        lint_for_leg_tests("out = df.filter(like=\"prefix_\")\nprint(out[\"zzz\"])\n");

    // assert
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
}

#[test]
fn test_should_narrow_to_an_exact_schema_through_select_variadic_pl_col_args() {
    // arrange: `select(pl.col("a"), pl.col("b"))` -- the variadic polars form, as
    // opposed to the already-supported single-list-literal `select(["a", "b"])`.
    let source = r#"
import polars as pl

class S:
    pass

df = pl.read_csv("x.csv", columns=["a", "b", "c"])
out = df.select(pl.col("a"), pl.col("b"))
print(out["a"])
print(out["c"])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert: narrowed to the exact {a, b}, not the base's full {a, b, c} --
    // exactly one error (the pre-existing pl.col validator and this narrowing must
    // not both separately flag the same bad reference).
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'c'"));
    assert!(errors[0].message.contains("{a, b}"));
}

#[test]
fn test_should_validate_a_string_literal_mixed_with_a_pl_col_arg_in_select() {
    // arrange: a bad bare string literal alongside a valid `pl.col(...)` arg in
    // the SAME call -- regression test for a bug where the presence of ANY
    // `pl.col()` arg skipped validating every OTHER arg's literal too, since
    // the generic `pl.col()` checker only ever looks at `pl.col()` names, never
    // string literals mixed in alongside them.
    let source = r#"
import polars as pl

df = pl.read_csv("x.csv", columns=["a", "b"])
out = df.select("bad_col", pl.col("a"))
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'bad_col'"));
}

#[test]
fn test_should_narrow_to_an_exact_schema_through_select_variadic_string_args() {
    // arrange: bare string positional args, `select("a", "b")` -- pandas has no
    // such form, but polars does, and it shares no code path with `pl.col(...)`.
    let (_, errors) = lint_for_leg_tests("out = df.select(\"a\", \"b\")\nprint(out[\"c\"])\n");

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'c'"));
    assert!(errors[0].message.contains("{a, b}"));
}

#[test]
fn test_should_mark_unresolved_after_a_select_this_checker_cannot_resolve() {
    // arrange: `pl.all()` selects every column dynamically -- not a knowable exact
    // set, and (the bug this fixes) must not be reported as a safe pass-through of
    // the base's full schema either.
    let source = r#"
import polars as pl

df = pl.read_csv("x.csv", columns=["a", "b"])
out = df.select(pl.all())
print(out["zzz"])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
}

#[test]
fn test_should_validate_chained_pl_col() {
    // arrange
    let source = r#"
from typing import Annotated
from typedframes import BaseSchema, Column
import polars as pl

class StockSchema(BaseSchema):
    ticker = Column(type=str)
    close = Column(type=float)

df: Annotated[pl.DataFrame, StockSchema] = pl.read_csv("stocks.csv")
result = df.filter(pl.col("close").is_not_null())
bad = df.filter(pl.col("open").is_not_null())
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(errors.len(), 1);
    assert!(errors[0].message.contains("open"));
}

#[test]
fn test_should_pass_valid_pl_col() {
    // arrange
    let source = r#"
from typing import Annotated
from typedframes import BaseSchema, Column
import polars as pl

class MetricsSchema(BaseSchema):
    date = Column(type=str)
    value = Column(type=float)

df: Annotated[pl.DataFrame, MetricsSchema] = pl.read_csv("metrics.csv")
filtered = df.filter(pl.col("value") > 100)
selected = df.select([pl.col("date"), pl.col("value")])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
}

#[test]
fn test_should_skip_pl_col_on_untracked_variable() {
    // arrange — variable has no schema (returned from opaque function), so no validation should occur
    let source = r#"
import polars as pl

df = some_function()
result = df.filter(pl.col("nonexistent_column") > 0)
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert — untracked variable, no column validation
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
}

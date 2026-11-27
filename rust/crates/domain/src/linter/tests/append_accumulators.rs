use super::*;

#[test]
fn test_should_infer_columns_from_an_append_loop_accumulator() {
    // arrange: `records = []` then `.append({literal dict})` calls in a loop --
    // the shape a `pd.DataFrame([{...}])` literal can't cover on its own.
    let source = r#"
import pandas as pd

records = []
for row in rows:
    records.append({"a": row.a, "b": row.b})

df = pd.DataFrame(records)
print(df["a"])
print(df["nope"])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(linter.dataframes_total, 1);
    assert_eq!(linter.dataframes_typed, 1);
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'nope'"));
    assert!(errors[0].message.contains("{a, b}"));
}

#[test]
fn test_should_union_keys_across_nested_loop_appends_in_first_seen_order() {
    // arrange: a nested loop, appending the same shape of dict every iteration --
    // the pattern this feature exists for.
    let source = r#"
import pandas as pd

records = []
for x in xs:
    for y in ys:
        records.append({"sku": x, "date": y, "price": 1})

df = pd.DataFrame(records)
print(df["sku"])
print(df["price"])
print(df["nope"])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'nope'"));
    assert!(errors[0].message.contains("{sku, date, price}"));
}

#[test]
fn test_should_leave_an_accumulator_untracked_when_mutated_by_another_function() {
    // arrange: `helper` could append anything -- a call we can't see into must not
    // be silently assumed harmless.
    let source = r#"
import pandas as pd

def helper(records):
    records.append({"weird_key": 1})

records = []
records.append({"a": 1})
helper(records)
df = pd.DataFrame(records)
print(df["a"])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert: no false unknown-column against an incomplete {a}-only schema.
    assert_eq!(linter.dataframes_typed, 0);
    assert!(
        errors
            .iter()
            .all(|e| e.code != CODE_UNKNOWN_COLUMN && e.code != CODE_UNVERIFIABLE_COLUMN),
        "errors: {errors:?}"
    );
}

#[test]
fn test_should_leave_an_accumulator_untracked_after_a_non_append_mutation() {
    // arrange
    let source = r#"
import pandas as pd

records = []
records.append({"a": 1})
records.extend([{"b": 2}])
df = pd.DataFrame(records)
print(df["a"])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(linter.dataframes_typed, 0);
    assert!(
        errors
            .iter()
            .all(|e| e.code != CODE_UNKNOWN_COLUMN && e.code != CODE_UNVERIFIABLE_COLUMN),
        "errors: {errors:?}"
    );
}

#[test]
fn test_should_leave_an_accumulator_untracked_after_an_append_with_a_dynamic_key() {
    // arrange
    let source = r#"
import pandas as pd

records = []
key = input()
records.append({key: 1})
df = pd.DataFrame(records)
print(df["a"])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(linter.dataframes_typed, 0);
    assert!(
        errors
            .iter()
            .all(|e| e.code != CODE_UNKNOWN_COLUMN && e.code != CODE_UNVERIFIABLE_COLUMN),
        "errors: {errors:?}"
    );
}

#[test]
fn test_should_leave_an_accumulator_untracked_after_a_read_only_reference() {
    // arrange: even a harmless-looking `len(records)` before construction poisons
    // it -- deliberately conservative, since distinguishing "definitely read-only"
    // calls from ones that might mutate isn't attempted.
    let source = r#"
import pandas as pd

records = []
records.append({"a": 1})
print(len(records))
df = pd.DataFrame(records)
print(df["a"])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(linter.dataframes_typed, 0);
    assert!(
        errors
            .iter()
            .all(|e| e.code != CODE_UNKNOWN_COLUMN && e.code != CODE_UNVERIFIABLE_COLUMN),
        "errors: {errors:?}"
    );
}

#[test]
fn test_should_leave_an_accumulator_untracked_after_being_reinitialized() {
    // arrange: two separate `records = []` bindings -- ambiguous which loop's
    // appends belong to "the" list by the time DataFrame(records) is reached.
    let source = r#"
import pandas as pd

records = []
records.append({"a": 1})
records = []
records.append({"b": 2})
df = pd.DataFrame(records)
print(df["b"])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(linter.dataframes_typed, 0);
    assert!(
        errors
            .iter()
            .all(|e| e.code != CODE_UNKNOWN_COLUMN && e.code != CODE_UNVERIFIABLE_COLUMN),
        "errors: {errors:?}"
    );
}

#[test]
fn test_should_not_infer_columns_for_an_unrelated_name_that_happens_to_hold_a_list() {
    // arrange: `items` is never initialized as `[]`, so it's simply never a
    // candidate -- not poisoned, just never tracked in the first place.
    let source = r#"
import pandas as pd

items = get_items()
df = pd.DataFrame(items)
print(df["anything"])
"#;
    let mut linter = Linter::new();

    // act
    let errors = linter
        .check_file_internal(source, Path::new("test.py"))
        .unwrap();

    // assert
    assert_eq!(linter.dataframes_typed, 0);
    assert!(
        errors
            .iter()
            .all(|e| e.code != CODE_UNKNOWN_COLUMN && e.code != CODE_UNVERIFIABLE_COLUMN),
        "errors: {errors:?}"
    );
}

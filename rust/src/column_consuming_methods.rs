//! The classification table Phase A4 uses to say whether a `.method()`/`.attribute`
//! seen on a [`crate::column_usage::OtherUse`] is known to read every column's
//! values, known to read none (pure shape/metadata), or can't be classified here.
//! `column_usage::UsageScan` calls [`classify_name`] at record time and stores the
//! verdict on the `OtherUse` itself as its `consumes` field -- see that module for
//! how the "unknown" case is then treated (conservatively, as still needing the
//! columns nothing else accounted for).

use crate::column_usage::Consumption;

/// Reads every column's actual values. `head`/`tail`/`describe`/`info` sample real
/// data despite not naming a specific column, so they're grouped here rather than
/// with the pure-metadata list below.
const WHOLE_FRAME_CONSUMERS: &[&str] = &[
    "to_dict",
    "to_csv",
    "to_parquet",
    "to_json",
    "to_sql",
    "to_records",
    "to_numpy",
    "to_excel",
    "to_html",
    "to_markdown",
    "to_clipboard",
    "to_feather",
    "values",
    "itertuples",
    "iterrows",
    "merge",
    "join",
    "copy",
    "head",
    "tail",
    "describe",
    "info",
];

/// Shape/metadata only -- never touches a column's actual values.
const NON_CONSUMERS: &[&str] = &["shape", "columns", "index", "dtypes", "size", "empty"];

/// Classify a `.method()`/`.attribute` name found on a tracked origin.
pub(crate) fn classify_name(name: &str) -> Consumption {
    if WHOLE_FRAME_CONSUMERS.contains(&name) {
        Consumption::All
    } else if NON_CONSUMERS.contains(&name) {
        Consumption::None
    } else {
        Consumption::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_classify_to_dict_as_a_whole_frame_consumer() {
        assert_eq!(classify_name("to_dict"), Consumption::All);
    }

    #[test]
    fn test_should_classify_columns_as_a_non_consumer() {
        assert_eq!(classify_name("columns"), Consumption::None);
    }

    #[test]
    fn test_should_classify_an_unrecognized_name_as_unknown() {
        assert_eq!(classify_name("some_unknown_method"), Consumption::Unknown);
    }

    #[test]
    fn test_should_classify_head_tail_describe_info_as_whole_frame_consumers() {
        for name in ["head", "tail", "describe", "info"] {
            assert_eq!(classify_name(name), Consumption::All);
        }
    }

    #[test]
    fn test_should_classify_every_non_consumer_name() {
        for name in ["shape", "columns", "index", "dtypes", "size", "empty"] {
            assert_eq!(classify_name(name), Consumption::None);
        }
    }
}

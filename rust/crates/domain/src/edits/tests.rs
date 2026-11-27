use super::*;

#[test]
fn test_should_apply_edits_from_the_end_so_offsets_stay_valid() {
    let edits = vec![
        Edit {
            start: 1,
            end: 1,
            replacement: "X".to_string(),
        },
        Edit {
            start: 3,
            end: 3,
            replacement: "Y".to_string(),
        },
    ];

    assert_eq!(apply_edits("abcd", &edits).unwrap(), "aXbcYd");
}

#[test]
fn test_should_refuse_overlapping_or_out_of_range_edits() {
    let overlapping = vec![
        Edit {
            start: 1,
            end: 3,
            replacement: String::new(),
        },
        Edit {
            start: 2,
            end: 2,
            replacement: "Z".to_string(),
        },
    ];
    let out_of_range = vec![Edit {
        start: 9,
        end: 9,
        replacement: "Z".to_string(),
    }];

    assert!(apply_edits("abcd", &overlapping).is_none());
    assert!(apply_edits("abcd", &out_of_range).is_none());
}

#[test]
fn test_should_count_an_error_swapped_for_another_as_new() {
    let before = vec!["unknown-column\0no column 'a' (line )".to_string()];
    let same = before.clone();
    let swapped = vec!["unknown-column\0no column 'b' (line )".to_string()];

    assert_eq!(new_errors(&before, &same), 0);
    assert_eq!(new_errors(&before, &swapped), 1);
    assert_eq!(new_errors(&before, &[]), 0);
    assert_eq!(new_errors(&[], &same), 1);
}

#[test]
fn test_should_ignore_line_numbers_when_comparing_errors() {
    let at = |line: usize| LintError {
        line,
        col: 1,
        code: "unknown-column".to_string(),
        message: format!("no column 'a' (defined at line {line})"),
        severity: "error".to_string(),
    };

    assert_eq!(error_keys(&[at(3)]), error_keys(&[at(14)]));
}

#[test]
fn test_should_only_strip_line_numbers_from_error_keys() {
    let error = |message: &str| LintError {
        line: 1,
        col: 1,
        code: "unknown-column".to_string(),
        message: message.to_string(),
        severity: "error".to_string(),
    };

    let q1 = error_keys(&[error("no column 'q1'")]);
    let q2 = error_keys(&[error("no column 'q2'")]);
    let moved_a = error_keys(&[error("defined at line 3 (a.py:3)")]);
    let moved_b = error_keys(&[error("defined at line 14 (a.py:14)")]);

    assert_ne!(q1, q2);
    assert_eq!(moved_a, moved_b);
}

#[test]
fn test_should_not_treat_other_text_before_a_number_as_a_line_reference() {
    assert_eq!(
        strip_line_numbers("see line 12 and a.py:34"),
        "see line  and a.py:"
    );
    assert_eq!(strip_line_numbers("pipeline 5 at x:7"), "pipeline 5 at x:7");
}

#[test]
fn test_should_apply_an_identical_shared_edit_once() {
    // arrange
    let shared = Edit {
        start: 0,
        end: 0,
        replacement: "import x\n".to_string(),
    };

    // act
    let edited = apply_edits("y = 1\n", &[shared.clone(), shared]);

    // assert
    assert_eq!(edited.as_deref(), Some("import x\ny = 1\n"));
}

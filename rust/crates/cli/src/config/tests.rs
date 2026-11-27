use super::*;

fn matches(pattern: &str, path: &str) -> bool {
    Glob::new(pattern).is_match(path)
}

#[test]
fn test_should_let_double_star_span_directories() {
    assert!(matches("legacy/**", "legacy/etl/load.py"));
    assert!(matches("legacy/**", "legacy/x.py"));
    assert!(!matches("legacy/**", "other/legacy/x.py"));
}

#[test]
fn test_should_match_a_root_file_with_a_leading_double_star_directory() {
    assert!(matches("**/x.py", "x.py"));
    assert!(matches("**/x.py", "a/b/x.py"));
}

#[test]
fn test_should_keep_single_star_and_question_mark_inside_one_segment() {
    assert!(matches("src/*.py", "src/a.py"));
    assert!(!matches("src/*.py", "src/sub/a.py"));
    assert!(matches("src/?.py", "src/a.py"));
    assert!(!matches("src/?.py", "src/ab.py"));
}

#[test]
fn test_should_treat_regex_characters_in_a_pattern_literally() {
    assert!(matches("a+b/(c).py", "a+b/(c).py"));
    assert!(!matches("a.py", "abpy"));
}

#[test]
fn test_should_match_a_trailing_double_star_directory_only_with_its_separator() {
    assert!(matches("src/**/", "src/a/"));
    assert!(matches("src/**/", "src/"));
    assert!(!matches("src/**/", "src"));
}

#[test]
fn test_should_treat_three_stars_as_a_double_star() {
    assert!(matches("a/***", "a/b/c.py"));
    assert!(matches("a/***/c.py", "a/c.py"));
}

#[test]
fn test_should_match_the_whole_path_not_a_part_of_it() {
    assert!(!matches("a", "ab"));
    assert!(!matches("a", "ba"));
    assert!(matches("", ""));
    assert!(!matches("", "a"));
}

#[test]
fn test_should_not_let_double_star_cross_a_line_break() {
    assert!(!matches("a/**", "a/b\nc"));
    assert!(matches("a/*", "a/b\nc"));
}

#[test]
fn test_should_count_characters_not_bytes_for_a_question_mark() {
    assert!(matches("src/?.py", "src/\u{e9}.py"));
}

#[test]
fn test_should_match_many_double_stars_in_linear_time() {
    let pattern = "**/".repeat(40) + "x.py";
    let path = "a/".repeat(200) + "y.py";
    assert!(!matches(&pattern, &path));
    assert!(matches(&pattern, &("a/".repeat(200) + "x.py")));
}

#[test]
fn test_should_rank_a_longer_literal_prefix_as_more_specific() {
    assert!(pattern_specificity("src/new_module/**") > pattern_specificity("src/**"));
    assert!(pattern_specificity("src/**/a") > pattern_specificity("src/**"));
}

#[test]
fn test_should_read_the_coverage_table_from_pyproject() {
    // arrange
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("pyproject.toml"),
        "[tool.typedframes.coverage]\nenabled = true\nfail_under = 80\ndetail = \"explain\"\n\
             [tool.typedframes.coverage.overrides]\n\"legacy/**\" = 50\n",
    )
    .unwrap();

    // act
    let config = load_coverage_config(dir.path());

    // assert
    assert!(config.enabled);
    assert_eq!(80.0, config.fail_under);
    assert_eq!(CoverageDetail::Explain, config.detail);
    assert_eq!(vec![("legacy/**".to_string(), 50.0)], config.overrides);
}

#[test]
fn test_should_prefer_a_standalone_file_over_pyproject_entirely() {
    // arrange
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("pyproject.toml"),
        "[tool.typedframes.coverage]\nenabled = true\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("typedframes.toml"),
        "[coverage]\nfail_under = 10\n",
    )
    .unwrap();

    // act
    let config = load_coverage_config(dir.path());

    // assert
    assert!(!config.enabled);
    assert_eq!(10.0, config.fail_under);
}

#[test]
fn test_should_give_the_disabled_default_for_a_single_file() {
    // arrange
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.py");
    std::fs::write(&file, "x = 1\n").unwrap();

    // act
    let config = load_coverage_config(&file);

    // assert
    assert!(!config.enabled);
}

#[test]
fn test_should_tell_no_exclude_key_from_an_empty_one() {
    // arrange
    let none = tempfile::tempdir().unwrap();
    std::fs::write(none.path().join("pyproject.toml"), "[tool.typedframes]\n").unwrap();
    let empty = tempfile::tempdir().unwrap();
    std::fs::write(
        empty.path().join("pyproject.toml"),
        "[tool.typedframes]\nexclude = []\n",
    )
    .unwrap();
    let some = tempfile::tempdir().unwrap();
    std::fs::write(
        some.path().join("pyproject.toml"),
        "[tool.typedframes]\nexclude = [\"legacy\", 3]\n",
    )
    .unwrap();

    // act
    let (a, b, c) = (
        load_configured_excludes(none.path()),
        load_configured_excludes(empty.path()),
        load_configured_excludes(some.path()),
    );

    // assert
    assert_eq!(None, a);
    assert_eq!(Some(HashSet::new()), b);
    assert_eq!(Some(HashSet::from(["legacy".to_string()])), c);
}

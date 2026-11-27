use super::*;

#[test]
fn test_should_keep_an_index_cache_where_told_and_clear_it_on_request() {
    // arrange
    let project = Project::new();
    project.write("a.py", "x = 1\n");
    let cache = project.path().join(".tf_cache");

    // act
    project.run(&["check", ".", "--cache-dir", cache.to_str().unwrap()]);
    let written = fs::read_dir(&cache).unwrap().count();
    let cleared = project.run(&[
        "cache",
        "clear",
        ".",
        "--cache-dir",
        cache.to_str().unwrap(),
    ]);

    // assert
    assert!(written >= 1);
    assert!(
        cleared.stdout.starts_with("Removed 1 cache file ("),
        "{}",
        cleared.stdout
    );
    assert_eq!(0, fs::read_dir(&cache).unwrap().count());
}

#[test]
fn test_should_suggest_for_several_paths_in_one_run() {
    // arrange
    let project = Project::new();
    project.write("one/job.py", SAFE_SUGGESTION);
    project.write("two/job.py", SAFE_SUGGESTION);

    // act
    let output = project.run(&["suggest", "one", "two"]);

    // assert
    assert_eq!(0, output.code, "{}{}", output.stdout, output.stderr);
    assert!(output.stdout.contains("one/job.py"), "{}", output.stdout);
    assert!(output.stdout.contains("two/job.py"), "{}", output.stdout);
}

#[test]
fn test_should_print_a_dry_run_with_a_named_suggestion_and_change_nothing() {
    // arrange
    let project = Project::new();
    project.write("job.py", SAFE_SUGGESTION);

    // act
    let output = project.run(&["suggest", "."]);

    // assert
    assert_eq!(0, output.code);
    assert!(output.stdout.contains("1 suggestion (1 safe, 0 review), 0 withheld, in 1 file. Dry run: no files were changed."), "{}", output.stdout);
    assert!(
        output.stdout.contains("+    df = df[[\"a\", \"b\"]]"),
        "{}",
        output.stdout
    );
    assert_eq!(SAFE_SUGGESTION, project.read("job.py"));
}

#[test]
fn test_should_apply_a_safe_suggestion_when_allowed_on_a_dirty_tree() {
    // arrange
    let project = Project::new();
    project.write("job.py", SAFE_SUGGESTION);

    // act
    let output = project.run(&["suggest", ".", "--apply", "--allow-dirty"]);

    // assert
    assert_eq!(0, output.code, "{}{}", output.stdout, output.stderr);
    assert!(
        output
            .stdout
            .contains("Applied 1 suggestion in 1 file, 0 skipped."),
        "{}",
        output.stdout
    );
    assert!(
        project.read("job.py").contains(
            "    df = df[[\"a\", \"b\"]]
"
        ),
        "{}",
        project.read("job.py")
    );
}

#[test]
fn test_should_refuse_to_edit_outside_a_clean_git_work_tree() {
    // arrange
    let project = Project::new();
    project.write("job.py", SAFE_SUGGESTION);

    // act
    let output = project.run(&["suggest", ".", "--apply"]);

    // assert
    assert_eq!(1, output.code);
    assert!(output.stderr.starts_with("Error: "), "{}", output.stderr);
    assert_eq!(SAFE_SUGGESTION, project.read("job.py"));
}

#[test]
fn test_should_apply_only_the_suggestions_answered_yes_when_interactive() {
    // arrange
    let project = Project::new();
    project.write(
        "job.py",
        "import pandas as pd\n\n\ndef first():\n    df = pd.read_json(\"x.json\")\n    return df[\"a\"]\n\n\ndef second():\n    df = pd.read_json(\"y.json\")\n    return df[\"b\"]\n",
    );

    // act
    let output = project.run_with_input(
        &["suggest", ".", "--interactive", "--allow-dirty"],
        "y\nn\n",
    );

    // assert
    assert_eq!(0, output.code, "{}{}", output.stdout, output.stderr);
    let edited = project.read("job.py");
    assert!(edited.contains("df = df[[\"a\"]]"), "{edited}");
    assert!(!edited.contains("df = df[[\"b\"]]"), "{edited}");
}

#[test]
fn test_should_quit_without_changes_when_the_answer_is_q_or_input_ends() {
    // arrange
    let project = Project::new();
    project.write("job.py", SAFE_SUGGESTION);

    // act
    let quit = project.run_with_input(&["suggest", ".", "-i", "--allow-dirty"], "q\n");
    let closed = project.run_with_input(&["suggest", ".", "-i", "--allow-dirty"], "");

    // assert
    assert!(quit.stdout.contains("Quit: nothing was changed."));
    assert!(closed.stdout.contains("Quit: nothing was changed."));
    assert_eq!(SAFE_SUGGESTION, project.read("job.py"));
}

#[test]
fn test_should_refuse_a_site_name_that_matches_nothing() {
    // arrange
    let project = Project::new();
    project.write("job.py", SAFE_SUGGESTION);

    // act
    let output = project.run(&["suggest", ".", "--only", "no-such"]);

    // assert
    assert_eq!(2, output.code);
    assert!(
        output
            .stderr
            .contains("Error: no untyped site is named 'no-such'"),
        "{}",
        output.stderr
    );
}

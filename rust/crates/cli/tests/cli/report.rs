use super::*;

fn first_site_name(project: &Project) -> String {
    let listing = project.run(&["suggest", ".", "--no-index"]).stdout;
    listing
        .lines()
        .find_map(|line| {
            let name = line.trim_start().split("  ").next()?;
            let (a, b) = name.split_once('-')?;
            (a.chars().all(|c| c.is_ascii_lowercase()) && b.chars().all(|c| c.is_ascii_lowercase()))
                .then(|| name.to_string())
        })
        .expect("a named site")
}

#[test]
fn test_should_report_a_site_without_the_original_names() {
    // arrange
    let project = Project::new();
    project.write("job.py", PRIVATE_SITE);
    let name = first_site_name(&project);

    // act
    let output = project.run(&["report", &name, ".", "--no-index"]);

    // assert
    assert_eq!(0, output.code, "{}", output.stderr);
    assert!(output.stdout.starts_with("<!-- Review before posting"));
    assert!(output
        .stdout
        .contains(&format!("## typedframes report: {name}")));
    assert!(output.stdout.contains("- Outcome: withheld"));
    assert!(output.stdout.contains("pd.read_csv("));
    for secret in [
        "acme",
        "customer",
        "orders",
        "gross_margin",
        "clean_the_data",
        "secret_loader",
    ] {
        assert!(
            !output.stdout.contains(secret),
            "{secret} leaked:\n{}",
            output.stdout
        );
    }
    assert!(output.stderr.contains("Read the report before posting it"));
}

#[test]
fn test_should_group_every_untyped_site_with_report_all_and_write_one_file_per_group() {
    // arrange
    let project = Project::new();
    project.write("job.py", PRIVATE_SITE);
    project.write("other.py", &PRIVATE_SITE.replace("acme", "zeta"));
    let out_dir = project.path().join("reports");

    // act
    let printed = project.run(&["report", "--all", ".", "--no-index"]);
    let written = project.run(&[
        "report",
        "--all",
        ".",
        "--no-index",
        "--output-dir",
        out_dir.to_str().unwrap(),
    ]);

    // assert
    assert!(
        printed
            .stdout
            .contains("## typedframes report: 2 untyped sites in 1 group"),
        "{}",
        printed.stdout
    );
    assert_eq!(1, fs::read_dir(&out_dir).unwrap().count());
    assert!(written.stdout.contains("01-"), "{}", written.stdout);
}

#[test]
fn test_should_print_one_short_entry_per_group_with_report_all_compact() {
    // arrange
    let project = Project::new();
    project.write("job.py", PRIVATE_SITE);

    // act
    let output = project.run(&["report", "--all", ".", "--no-index", "--format", "compact"]);

    // assert
    assert!(
        output.stdout.contains("1 site, 1 group"),
        "{}",
        output.stdout
    );
    assert!(output.stdout.contains("1. x1 "), "{}", output.stdout);
    assert!(!output.stdout.contains("<!--"), "{}", output.stdout);
    assert!(output.stdout.lines().count() < 20, "{}", output.stdout);
}

#[test]
fn test_should_say_there_are_no_untyped_sites_when_there_are_none() {
    // arrange
    let project = Project::new();
    project.write("ok.py", "x = 1\n");

    // act
    let output = project.run(&["report", "--all", "."]);

    // assert
    assert_eq!("No untyped sites.\n", output.stdout);
}

#[test]
fn test_should_exit_2_when_report_has_neither_a_name_nor_all() {
    // arrange
    let project = Project::new();

    // act
    let output = project.run(&["report", "--no-index"]);

    // assert
    assert_eq!(2, output.code);
    assert!(output
        .stderr
        .contains("Error: `report` needs a site name, or --all"));
}

const UNTRACKED_SITE: &str =
    "def run(client):\n    rows = client.fetch(\"q\")\n    return rows[\"price\"].sum()\n";

#[test]
fn test_should_list_frames_the_checker_does_not_track_with_outcome_untracked() {
    // arrange
    let project = Project::new();
    project.write("job.py", UNTRACKED_SITE);

    // act
    let output = project.run(&[
        "report",
        "--all",
        ".",
        "--no-index",
        "--outcome",
        "untracked",
    ]);

    // assert
    assert_eq!(0, output.code, "{}", output.stderr);
    assert!(
        output.stdout.contains("1 untracked frame in 1 group"),
        "{}",
        output.stdout
    );
    assert!(
        output.stdout.contains("untracked: it comes from"),
        "{}",
        output.stdout
    );
    assert!(!output.stdout.contains("price"), "{}", output.stdout);
}

#[test]
fn test_should_say_there_are_no_untracked_frames_when_there_are_none() {
    // arrange
    let project = Project::new();
    project.write("ok.py", "x = 1\n");

    // act
    let output = project.run(&["report", "--all", ".", "--outcome", "untracked"]);

    // assert
    assert_eq!("No untracked frames.\n", output.stdout);
}

#[test]
fn test_should_exit_2_when_report_outcome_is_given_without_all() {
    // arrange
    let project = Project::new();

    // act
    let output = project.run(&["report", "some-name", "--outcome", "withheld"]);

    // assert
    assert_eq!(2, output.code);
    assert!(output.stderr.contains("Error: `--outcome` needs --all"));
}

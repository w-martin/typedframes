use super::*;

#[test]
fn test_should_find_the_same_called_functions_with_a_warm_cache_as_without_one() {
    // arrange
    let project = project_with(&[
        ("a.py", LOADER),
        (
            "b.py",
            "from a import load\n\n\ndef f():\n    d = load(\"x\")\n    return d\n",
        ),
    ]);
    let cache = tempfile::tempdir().unwrap();
    let dir_cache = DirCache(cache.path().to_path_buf());

    // act
    let uncached = build_index_cached(&OsFiles, project.path(), None);
    build_index_cached(&OsFiles, project.path(), Some(&dir_cache));
    let warm = build_index_cached(&OsFiles, project.path(), Some(&dir_cache));

    // assert
    assert!(!uncached.called_functions.is_empty());
    assert_eq!(uncached.called_functions, warm.called_functions);
}

#[test]
fn test_should_build_the_same_index_with_a_warm_cache_as_without_one() {
    let project = project_with(&[("a.py", LOADER), ("b.py", "x = 1\n")]);
    let cache = tempfile::tempdir().unwrap();
    let dir_cache = DirCache(cache.path().to_path_buf());

    let uncached = build_index_cached(&OsFiles, project.path(), None);
    let cold = build_index_cached(&OsFiles, project.path(), Some(&dir_cache));
    let warm = build_index_cached(&OsFiles, project.path(), Some(&dir_cache));

    assert_eq!(as_json(&uncached), as_json(&cold));
    assert_eq!(as_json(&uncached), as_json(&warm));
}

#[test]
fn test_should_reuse_the_entry_of_an_unchanged_file() {
    let project = project_with(&[("a.py", LOADER), ("b.py", "x = 1\n")]);
    let cache = tempfile::tempdir().unwrap();
    let dir_cache = DirCache(cache.path().to_path_buf());
    let config = LinterConfig::EMPTY;
    let first = FileIndexer::new(&OsFiles, project.path(), &config, Some(&dir_cache));
    let paths = [project.path().join("a.py"), project.path().join("b.py")];
    first.index_all(&paths);
    first.save(true);

    let second = FileIndexer::new(&OsFiles, project.path(), &config, Some(&dir_cache));
    second.index_all(&paths);

    assert_eq!(second.loaded.len(), 2);
    let slots = second.slots.lock().unwrap();
    assert!(slots.values().all(|s| matches!(s, Slot::Kept)));
}

#[test]
fn test_should_reindex_only_the_file_that_changed() {
    let project = project_with(&[("a.py", LOADER), ("b.py", "x = 1\n")]);
    let cache = tempfile::tempdir().unwrap();
    let dir_cache = DirCache(cache.path().to_path_buf());
    build_index_cached(&OsFiles, project.path(), Some(&dir_cache));
    fs::write(
        project.path().join("a.py"),
        LOADER.replace("[\"a\", \"b\"]", "[\"x\", \"y\"]"),
    )
    .unwrap();

    let rebuilt = build_index_cached(&OsFiles, project.path(), Some(&dir_cache));

    let entry = rebuilt
        .files
        .get(project.path().join("a.py").to_str().unwrap())
        .unwrap();
    let schema = entry.functions["load"].returns_schema.clone();
    assert_eq!(entry.schemas[&schema], vec!["x", "y"]);
    let uncached = build_index_cached(&OsFiles, project.path(), None);
    assert_eq!(as_json(&rebuilt), as_json(&uncached));
}

#[test]
fn test_should_drop_a_deleted_file_from_the_cache() {
    let project = project_with(&[("a.py", LOADER), ("b.py", "x = 1\n")]);
    let cache = tempfile::tempdir().unwrap();
    let dir_cache = DirCache(cache.path().to_path_buf());
    build_index_cached(&OsFiles, project.path(), Some(&dir_cache));
    std::fs::remove_file(project.path().join("b.py")).unwrap();

    let rebuilt = build_index_cached(&OsFiles, project.path(), Some(&dir_cache));

    assert_eq!(rebuilt.files.len(), 1);
    let config = LinterConfig::EMPTY;
    let after = FileIndexer::new(&OsFiles, project.path(), &config, Some(&dir_cache));
    assert_eq!(after.loaded.len(), 1);
}

#[test]
fn test_should_ignore_a_cache_made_under_a_different_configuration() {
    let project = project_with(&[("a.py", LOADER)]);
    let cache = tempfile::tempdir().unwrap();
    let dir_cache = DirCache(cache.path().to_path_buf());
    build_index_cached(&OsFiles, project.path(), Some(&dir_cache));
    fs::write(
        project.path().join("pyproject.toml"),
        "[tool.typedframes]\nwarnings = false\n",
    )
    .unwrap();

    let config = load_linter_config(&OsFiles, project.path());
    let indexer = FileIndexer::new(&OsFiles, project.path(), &config, Some(&dir_cache));

    assert!(indexer.loaded.is_empty());
}

#[test]
fn test_should_ignore_an_unreadable_cache_and_replace_it() {
    let project = project_with(&[("a.py", LOADER)]);
    let cache = tempfile::tempdir().unwrap();
    let dir_cache = DirCache(cache.path().to_path_buf());
    build_index_cached(&OsFiles, project.path(), Some(&dir_cache));
    let cache_file = fs::read_dir(cache.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::write(&cache_file, b"not a cache").unwrap();

    let rebuilt = build_index_cached(&OsFiles, project.path(), Some(&dir_cache));

    assert_eq!(rebuilt.files.len(), 1);
    let config = LinterConfig::EMPTY;
    let again = FileIndexer::new(&OsFiles, project.path(), &config, Some(&dir_cache));
    assert_eq!(again.loaded.len(), 1);
}

#[test]
fn test_should_not_cache_a_file_whose_index_depends_on_a_sql_file() {
    let source = "import pandas as pd\nfrom pathlib import Path\n\nsql = Path(\"orders.sql\").read_text()\ndf = pd.read_sql(sql, conn)\n";
    let project = project_with(&[("etl.py", source)]);
    fs::write(project.path().join("orders.sql"), "SELECT a FROM t").unwrap();
    let cache = tempfile::tempdir().unwrap();
    let dir_cache = DirCache(cache.path().to_path_buf());

    build_index_cached(&OsFiles, project.path(), Some(&dir_cache));

    let config = LinterConfig::EMPTY;
    let after = FileIndexer::new(&OsFiles, project.path(), &config, Some(&dir_cache));
    assert!(after.loaded.is_empty());
}

#[test]
fn test_should_keep_the_rest_of_the_cache_after_a_single_file_run() {
    let project = project_with(&[("a.py", LOADER), ("b.py", "x = 1\n"), ("c.py", "y = 2\n")]);
    let cache = tempfile::tempdir().unwrap();
    let dir_cache = DirCache(cache.path().to_path_buf());
    build_index_cached(&OsFiles, project.path(), Some(&dir_cache));

    build_single_file_index_cached(&OsFiles, &project.path().join("b.py"), Some(&dir_cache))
        .unwrap();

    let config = LinterConfig::EMPTY;
    let after = FileIndexer::new(&OsFiles, project.path(), &config, Some(&dir_cache));
    assert_eq!(after.loaded.len(), 3);
}

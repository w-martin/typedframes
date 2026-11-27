use super::*;

#[test]
fn test_should_only_record_what_callers_read_when_asked_to() {
    // arrange
    let project = project_with(&[
            ("lib.py", "import pandas as pd\n\n\ndef load():\n    df = pd.read_csv(\"x.csv\")\n    return df\n"),
            ("use.py", "from lib import load\n\n\ndef f():\n    d = load()\n    return d[\"a\"]\n"),
        ]);

    // act
    let without = build_index_cached(&OsFiles, project.path(), None);
    let with = build_index_with_callers(&OsFiles, project.path(), None);

    // assert
    assert!(without.call_usage.is_empty());
    assert_eq!(with.call_usage["load"].columns, vec!["a".to_string()]);
    assert_eq!(with.call_usage["load"].calls, 1);
}

#[cfg(unix)]
#[test]
fn test_should_still_follow_a_symlink_to_a_directory_when_collecting_python_files() {
    // arrange
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("real")).unwrap();
    fs::write(dir.path().join("real").join("a.py"), "x = 1\n").unwrap();
    fs::write(dir.path().join("b.py"), "y = 2\n").unwrap();
    fs::write(dir.path().join("notes.txt"), "not python\n").unwrap();
    std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("link")).unwrap();

    // act
    let mut found = collect_py_files(&OsFiles, dir.path(), &std::collections::HashSet::new());
    found.sort();

    // assert
    let names: Vec<String> = found
        .iter()
        .map(|p| p.strip_prefix(dir.path()).unwrap().display().to_string())
        .collect();
    assert_eq!(names, vec!["b.py", "link/a.py", "real/a.py"]);
}

#[test]
fn test_should_not_carry_call_sites_in_the_finished_index() {
    // arrange
    let project = project_with(&[
        ("a.py", LOADER),
        (
            "b.py",
            "from a import load\n\n\ndef f():\n    d = load(\"x\")\n    return d\n",
        ),
    ]);

    // act
    let index = build_index_cached(&OsFiles, project.path(), None);

    // assert: they were used (`load` is known to be called) and then dropped.
    let a_path = project.path().join("a.py").to_str().unwrap().to_string();
    assert!(index.called_functions[&a_path].contains("load"));
    assert!(index
        .files
        .values()
        .all(|entry| entry.call_sites.is_empty()));
}

#[test]
fn test_should_find_a_method_called_through_a_self_attribute_in_another_file() {
    // arrange
    let project = project_with(&[
            (
                "svc.py",
                "import pandas as pd\n\n\nclass Backtester:\n    def run(self) -> pd.DataFrame:\n        return pd.DataFrame({\"a\": [1]})\n",
            ),
            (
                "use.py",
                "from svc import Backtester\n\n\nclass Report:\n    def __init__(self):\n        self.bt = Backtester()\n\n    def go(self):\n        out = self.bt.run()\n        return out\n",
            ),
        ]);

    // act
    let index = build_index_cached(&OsFiles, project.path(), None);

    // assert
    let svc = project.path().join("svc.py").to_str().unwrap().to_string();
    assert!(index.called_functions[&svc].contains("run"));
}

#[test]
fn test_should_record_the_library_each_class_method_returns() {
    // arrange
    let project = project_with(&[(
            "repo.py",
            "import pandas as pd\nimport polars as pl\n\n\nclass Repo:\n    def orders(self) -> pd.DataFrame:\n        return pd.read_csv(\"o.csv\")\n\n    def users(self) -> pl.DataFrame:\n        return pl.read_csv(\"u.csv\")\n\n    def name(self) -> str:\n        return \"r\"\n",
        )]);

    // act
    let index = build_index_cached(&OsFiles, project.path(), None);

    // assert
    let repo = project.path().join("repo.py").to_str().unwrap().to_string();
    let methods = &index.files[&repo].class_method_libraries["Repo"];
    assert_eq!(methods.get("orders").map(String::as_str), Some("pandas"));
    assert_eq!(methods.get("users").map(String::as_str), Some("polars"));
    assert!(!methods.contains_key("name"));
}

#[test]
fn test_should_record_calls_in_every_statement_block() {
    // arrange
    let source = "\
if a:\n    in_if()\nelse:\n    in_else()\n\
for _ in b:\n    in_for()\n\
while c:\n    in_while()\n\
with d:\n    in_with()\n\
try:\n    in_try()\nexcept E:\n    in_except()\nelse:\n    in_orelse()\nfinally:\n    in_finally()\n";

    // act
    let sites = call_sites_of(source);

    // assert
    let expected = [
        "in_else",
        "in_except",
        "in_finally",
        "in_for",
        "in_if",
        "in_orelse",
        "in_try",
        "in_while",
        "in_with",
    ];
    assert_eq!(sites.names, expected);
}

#[test]
fn test_should_record_assigned_calls_attribute_callees_and_nested_function_calls() {
    // arrange
    let source = "x = make()\nmod.helper(x)\n\n\ndef outer():\n    def inner():\n        deep()\n";

    // act
    let sites = call_sites_of(source);

    // assert
    assert_eq!(sites.names, ["deep", "helper", "make"]);
}

#[test]
fn test_should_not_record_calls_that_are_not_statement_level() {
    // arrange
    let source = "y = [make() for _ in z]\nprint(other())\nvalue = (lambda: hidden())()\n";

    // act
    let sites = call_sites_of(source);

    // assert: only the outer `print(...)` call is a statement; `other` is an argument.
    assert_eq!(sites.names, ["print"]);
}

#[test]
fn test_should_give_a_nested_class_its_own_self_attribute_origins() {
    // arrange
    let source = "\
class Outer:\n    def __init__(self):\n        self.a = Alpha()\n\n    def run(self):\n        self.a.go()\n\
\n    class Inner:\n        def run(self):\n            self.a.nested()\n";

    // act
    let sites = call_sites_of(source);

    // assert: `Inner` has no `__init__`, so `self.a` is not an origin there.
    let methods: Vec<&str> = sites.self_calls.iter().map(|(_, m)| m.as_str()).collect();
    assert_eq!(methods, ["go"]);
}

#[test]
fn test_should_record_a_repeated_self_call_once() {
    // arrange
    let source = "\
class C:\n    def __init__(self):\n        self.a = Alpha()\n\n    def run(self):\n        self.a.go()\n        self.a.go()\n";

    // act
    let sites = call_sites_of(source);

    // assert
    assert_eq!(sites.self_calls.len(), 1);
}

#[test]
fn test_should_leave_no_call_sites_for_a_file_that_does_not_parse() {
    // arrange
    let project = project_with(&[("a.py", LOADER), ("broken.py", "def f(:\n    pass\n")]);
    let config = LinterConfig::EMPTY;
    let indexer = FileIndexer::new(&OsFiles, project.path(), &config, None);

    // act
    indexer.index_all(&[
        project.path().join("a.py"),
        project.path().join("broken.py"),
    ]);

    // assert
    let slots = indexer.slots.lock().unwrap();
    let broken = project.path().join("broken.py");
    let has_sites = slots.iter().any(|(path, slot)| {
        path == &broken && matches!(slot, Slot::Fresh(file) if !file.entry.call_sites.is_empty())
    });
    assert!(!has_sites);
}

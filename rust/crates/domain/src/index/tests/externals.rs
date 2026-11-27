use super::*;

#[test]
fn test_should_cap_auto_discovered_candidates_but_never_force_included_ones() {
    // arrange: 25 auto-discovered candidates (more than MAX_AUTO_TRACED_PACKAGES),
    // plus one package named in trace_external_packages that would sort dead last
    // alphabetically -- it must still appear in the result, uncapped.
    let config = LinterConfig {
        enabled: None,
        warnings: None,
        sql_dialect: None,
        trace_external_packages: Some(vec!["zzz_forced_pkg".to_string()]),
        excluded_external_packages: None,
        exclude: None,
    };
    let candidates: std::collections::HashSet<String> =
        (0..25).map(|i| format!("pkg_{i:02}")).collect();
    let dir = tempfile::tempdir().unwrap();

    // act
    let result = resolve_traced_external_packages(
        &OsFiles,
        &config,
        candidates,
        dir.path(),
        &HashMap::new(),
    );

    // assert: forced package present regardless of the cap ...
    assert!(result.contains(&"zzz_forced_pkg".to_string()));
    // ... and the auto-discovered portion is capped at MAX_AUTO_TRACED_PACKAGES,
    // keeping the alphabetically-first candidates for determinism.
    let auto_portion: Vec<&String> = result
        .iter()
        .filter(|name| *name != "zzz_forced_pkg")
        .collect();
    assert_eq!(auto_portion.len(), MAX_AUTO_TRACED_PACKAGES);
    assert!(auto_portion.contains(&&"pkg_00".to_string()));
    assert!(!auto_portion.contains(&&format!("pkg_{:02}", 24)));
}

#[test]
fn test_should_respect_excluded_external_packages_in_resolve_traced_external_packages() {
    // arrange
    let config = LinterConfig {
        enabled: None,
        warnings: None,
        sql_dialect: None,
        trace_external_packages: None,
        excluded_external_packages: Some(vec!["untrusted_pkg".to_string()]),
        exclude: None,
    };
    let candidates: std::collections::HashSet<String> =
        ["untrusted_pkg".to_string(), "fine_pkg".to_string()]
            .into_iter()
            .collect();
    let dir = tempfile::tempdir().unwrap();

    // act
    let result = resolve_traced_external_packages(
        &OsFiles,
        &config,
        candidates,
        dir.path(),
        &HashMap::new(),
    );

    // assert
    assert!(!result.contains(&"untrusted_pkg".to_string()));
    assert!(result.contains(&"fine_pkg".to_string()));
}

#[test]
fn test_should_never_treat_a_first_party_module_as_an_external_candidate() {
    // arrange: "utils" resolves to a real first-party file already in the index --
    // a bare `import utils` looking momentarily unresolved (before cross-file
    // symbols are loaded) must never be treated as an external-tracing candidate.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let config = LinterConfig::EMPTY;
    let mut first_party_files = HashMap::new();
    first_party_files.insert(
        root.join("utils.py").to_str().unwrap().to_string(),
        IndexEntry {
            schemas: HashMap::new(),
            schema_locations: HashMap::new(),
            functions: HashMap::new(),
            exports: Vec::new(),
            imports: HashMap::new(),
            module_aliases: HashMap::new(),
            class_methods: HashMap::new(),
            class_method_libraries: HashMap::new(),
            class_bases: HashMap::new(),
            class_helper_summaries: HashMap::new(),
            eval: Default::default(),
            foreign_writes: Vec::new(),
            import_targets: Vec::new(),
            call_sites: CallSites::default(),
            contracts: HashMap::new(),
            contract_forwards: Vec::new(),
            class_method_names: HashMap::new(),
            protocol_classes: Vec::new(),
        },
    );
    let candidates: std::collections::HashSet<String> = ["utils".to_string()].into_iter().collect();

    // act
    let result =
        resolve_traced_external_packages(&OsFiles, &config, candidates, root, &first_party_files);

    // assert
    assert!(result.is_empty());
}

#[test]
fn test_should_not_hang_on_mutually_delegating_functions() {
    // arrange: f and g delegate to each other — a cycle in the delegate graph.
    // Resolution must terminate (cycle guard) rather than recursing forever.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    fs::write(
        root.join("steps.py"),
        r#"
import pandas as pd

def f(df: pd.DataFrame) -> pd.DataFrame:
    x = df["a"]
    return g(df)

def g(df: pd.DataFrame) -> pd.DataFrame:
    y = df["b"]
    return f(df)
"#,
    )
    .unwrap();

    // act
    let index = build_index_internal(root);

    // assert: both functions resolved without hanging, each contributing at
    // least its own direct requirement.
    let steps_path = root.join("steps.py").to_str().unwrap().to_string();
    let entry = index.files.get(&steps_path).expect("steps.py indexed");
    let f_requires = &entry.functions.get("f").expect("f indexed").requires;
    let g_requires = &entry.functions.get("g").expect("g indexed").requires;
    assert!(f_requires.contains(&"a".to_string()));
    assert!(g_requires.contains(&"b".to_string()));
}

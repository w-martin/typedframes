use super::*;

// Union schema name -> column list across every file's `schemas` map. First
// definition wins on name collision (matches the prior per-call behaviour in both
// resolve_param_schema_requires and load_cross_file_symbols).
pub(crate) fn compute_all_schemas(
    files: &HashMap<String, IndexEntry>,
) -> HashMap<String, Vec<String>> {
    let mut all_schemas: HashMap<String, Vec<String>> = HashMap::new();
    let ambiguous = ambiguous_schema_names(files);
    // Sorted by path so a schema name defined in two files always resolves the same way.
    let mut ordered: Vec<(&String, &IndexEntry)> = files.iter().collect();
    ordered.sort_by(|a, b| a.0.cmp(b.0));
    for (_, entry) in ordered {
        for (name, cols) in &entry.schemas {
            if ambiguous.contains(name) {
                continue;
            }
            all_schemas
                .entry(name.clone())
                .or_insert_with(|| cols.clone());
        }
    }
    all_schemas
}

// Schema class names that two files define with different columns: the bare name cannot say
// which one a reference means (two packages in one tree can both have a `TestRaw`), so it
// resolves to neither.
fn ambiguous_schema_names(
    files: &HashMap<String, IndexEntry>,
) -> std::collections::HashSet<String> {
    let mut seen: HashMap<&str, &Vec<String>> = HashMap::new();
    let mut ambiguous = std::collections::HashSet::new();
    for entry in files.values() {
        for (name, cols) in &entry.schemas {
            match seen.get(name.as_str()) {
                Some(first) if *first != cols => {
                    ambiguous.insert(name.clone());
                }
                Some(_) => {}
                None => {
                    seen.insert(name.as_str(), cols);
                }
            }
        }
    }
    ambiguous
}

// Union schema name -> (file, definition line) across every file's `schema_locations`
// map, mirroring compute_all_schemas -- a schema's class may live in a THIRD file,
// neither the function's own file nor the one importing it, same reasoning as
// `all_schemas` itself.
pub(crate) fn compute_all_schema_locations(
    files: &HashMap<String, IndexEntry>,
) -> HashMap<String, (String, usize)> {
    let mut all_schema_locations: HashMap<String, (String, usize)> = HashMap::new();
    let ambiguous = ambiguous_schema_names(files);
    let mut ordered: Vec<(&String, &IndexEntry)> = files.iter().collect();
    ordered.sort_by(|a, b| a.0.cmp(b.0));
    for (_, entry) in ordered {
        for (name, loc) in &entry.schema_locations {
            if ambiguous.contains(name) {
                continue;
            }
            all_schema_locations
                .entry(name.clone())
                .or_insert_with(|| loc.clone());
        }
    }
    all_schema_locations
}

// Ceiling on how many AUTO-discovered candidate packages (see
// `discover_external_package_candidates`) get traced by default. Never applies to
// `trace_external_packages` -- an explicit force-include is never capped, since the
// user opted into it deliberately and by name. Bounds the worst case for a project
// whose first-party code happens to call many different external packages in
// DataFrame-shaped ways, in the same spirit as the MAX_SQL_FILE_READS_PER_FILE /
// MAX_SQL_FILE_BYTES guardrails in linter.rs's own `read_sql_file`.
pub(crate) const MAX_AUTO_TRACED_PACKAGES: usize = 20;

// Merge `trace_external_packages` (forced, uncapped, exempt from
// `excluded_external_packages`), auto-discovered candidates (capped at
// MAX_AUTO_TRACED_PACKAGES, alphabetically for determinism, filtered by
// `excluded_external_packages` and by not already being a first-party module), into
// the final list of package names to trace.
pub(crate) fn resolve_traced_external_packages(
    fs: &dyn crate::ports::FileSystem,
    config: &LinterConfig,
    discovered_candidates: std::collections::HashSet<String>,
    project_root: &Path,
    first_party_files: &HashMap<String, IndexEntry>,
) -> Vec<String> {
    let forced: std::collections::HashSet<String> = config
        .trace_external_packages
        .clone()
        .unwrap_or_default()
        .into_iter()
        .collect();
    let excluded: std::collections::HashSet<String> = config
        .excluded_external_packages
        .clone()
        .unwrap_or_default()
        .into_iter()
        .collect();

    let mut auto_candidates: Vec<String> = discovered_candidates
        .into_iter()
        .filter(|name| !forced.contains(name))
        .filter(|name| !excluded.contains(name))
        // A candidate that resolves to a first-party module isn't external at all --
        // e.g. a plain `import utils` inside a `src/` layout project can look exactly
        // like an unresolved external call before cross-file symbols are loaded.
        .filter(|name| resolve_module_file(fs, name, project_root, first_party_files).is_none())
        .collect();
    auto_candidates.sort();
    auto_candidates.truncate(MAX_AUTO_TRACED_PACKAGES);

    let mut result: Vec<String> = forced.into_iter().collect();
    result.extend(auto_candidates);
    result.sort();
    result.dedup();
    result
}

// Build a ProjectIndex by indexing every `.py` file under `project_root`. Two-phase:
// first-party files are indexed on their own (collecting external-package candidates
// along the way, see `index_file`), THEN the resolved external-package list is indexed
// and merged in — see `resolve_traced_external_packages`. This ordering is required,
// not just convenient: deciding which external packages are worth tracing depends on
// having already seen every first-party file's own imports and DataFrame-shaped usage.
#[cfg(test)]
pub(crate) fn build_index_internal(project_root: &Path) -> ProjectIndex {
    build_index_cached(&crate::ports::test_files::OsFiles, project_root, None)
}

// Builds the project index, using the per-file `cache` when given.
pub fn build_index_cached(
    fs: &dyn crate::ports::FileSystem,
    project_root: &Path,
    cache: Option<&dyn crate::ports::IndexCache>,
) -> ProjectIndex {
    build_index_inner(fs, project_root, cache, false)
}

// `build_index_cached` plus `ProjectIndex::call_usage`, which re-reads every file once more
// and is only worth it for the commands that narrow what a function returns.
pub fn build_index_with_callers(
    fs: &dyn crate::ports::FileSystem,
    project_root: &Path,
    cache: Option<&dyn crate::ports::IndexCache>,
) -> ProjectIndex {
    build_index_inner(fs, project_root, cache, true)
}

fn build_index_inner(
    fs: &dyn crate::ports::FileSystem,
    project_root: &Path,
    cache: Option<&dyn crate::ports::IndexCache>,
    with_callers: bool,
) -> ProjectIndex {
    // Loaded once and threaded into every per-file `Linter`, so index-time inference
    // (this function) and check-time inference (`check_file`) agree on `sql_dialect` —
    // a mismatch here would make a SQL-derived schema's columns differ depending on
    // whether they were read from the cached project index or inferred fresh.
    let config = load_linter_config(fs, project_root);
    let py_files = collect_py_files(fs, project_root, &resolve_excluded_dirs(&config));

    let mut files = HashMap::new();
    let mut discovered_candidates: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    let indexer = FileIndexer::new(fs, project_root, &config, cache);
    for (file_path, indexed) in py_files.iter().zip(indexer.index_all(&py_files)) {
        if let Some((entry, candidates)) = indexed {
            discovered_candidates.extend(candidates);
            if let Some(path_str) = file_path.to_str() {
                files.insert(path_str.to_string(), entry);
            }
        }
    }

    index_traced_external_packages(
        fs,
        project_root,
        &config,
        discovered_candidates,
        &mut files,
        &indexer,
    );
    indexer.save(true);
    finalise_index(fs, project_root, files, true, with_callers)
}

// A configured `exclude` REPLACES DEFAULT_EXCLUDED_DIRS entirely -- it does not add to
// it (see collect_py_files's doc comment).
pub(crate) fn resolve_excluded_dirs(config: &LinterConfig) -> std::collections::HashSet<String> {
    match &config.exclude {
        Some(list) => list.iter().cloned().collect(),
        None => DEFAULT_EXCLUDED_DIRS
            .iter()
            .map(|s| s.to_string())
            .collect(),
    }
}

// Second indexing phase, shared by the whole-project and single-file entry points:
// resolve which external installed packages are worth tracing (see
// `resolve_traced_external_packages` -- pure decision logic over an already-collected
// candidate set, so it works identically whether those candidates came from every file
// in the project or from just one file's own import/call graph), then index their files
// into the same map.
pub(crate) fn index_traced_external_packages(
    fs: &dyn crate::ports::FileSystem,
    project_root: &Path,
    config: &LinterConfig,
    discovered_candidates: std::collections::HashSet<String>,
    files: &mut HashMap<String, IndexEntry>,
    indexer: &FileIndexer,
) {
    let traced_packages =
        resolve_traced_external_packages(fs, config, discovered_candidates, project_root, files);
    let external_files = collect_external_package_files(fs, project_root, &traced_packages);
    for (file_path, indexed) in external_files
        .iter()
        .zip(indexer.index_all(&external_files))
    {
        if let Some((entry, _candidates)) = indexed {
            if let Some(path_str) = file_path.to_str() {
                files.insert(path_str.to_string(), entry);
            }
        }
    }
}

// A constant another indexed file may change (`COLS.append(..)` after importing it, or
// `cfg.NAME = ..`) is no longer offered to importers; through re-exports too.
fn apply_foreign_writes(
    fs: &dyn crate::ports::FileSystem,
    project_root: &Path,
    files: &mut HashMap<String, IndexEntry>,
) {
    let no_excluded = std::collections::HashSet::new();
    let mut removals: Vec<(String, String, bool)> = Vec::new();
    for (importer, entry) in files.iter_mut() {
        let writes = std::mem::take(&mut entry.foreign_writes);
        for write in writes {
            let Some(target) = resolve_import_path(
                fs,
                write.level,
                &write.module,
                Path::new(importer),
                project_root,
                &no_excluded,
            ) else {
                continue;
            };
            if let Some(target) = target.to_str() {
                removals.push((target.to_string(), write.name, write.rebinds));
            }
        }
    }
    for (file, name, rebinds) in removals {
        let mut at = (file, name);
        for _ in 0..4 {
            let Some(entry) = files.get_mut(&at.0) else {
                break;
            };
            let mutable = entry
                .eval
                .constants
                .get(&at.1)
                .is_some_and(|v| !v.is_immutable());
            if rebinds || mutable {
                entry.eval.constants.remove(&at.1);
            }
            let Some((level, module, imported)) = entry.eval.reexports.get(&at.1).cloned() else {
                break;
            };
            let Some(next) = resolve_import_path(
                fs,
                level,
                &module,
                Path::new(&at.0),
                project_root,
                &no_excluded,
            ) else {
                break;
            };
            let Some(next) = next.to_str() else {
                break;
            };
            at = (next.to_string(), imported);
        }
    }
}

// Final resolution pass, shared by the whole-project and single-file entry points.
// Every step here is a fold over the `files` map that was handed in — it never goes
// looking for more files — so it costs whatever that map contains and nothing more,
// which is exactly what lets single-file mode reuse it unchanged.
pub(crate) fn finalise_index(
    fs: &dyn crate::ports::FileSystem,
    project_root: &Path,
    mut files: HashMap<String, IndexEntry>,
    whole_project: bool,
    with_callers: bool,
) -> ProjectIndex {
    apply_foreign_writes(fs, project_root, &mut files);
    let all_schemas = compute_all_schemas(&files);
    let all_schema_locations = compute_all_schema_locations(&files);
    resolve_param_schema_requires(&mut files, &all_schemas);
    resolve_transitive_requires(fs, project_root, &mut files);
    resolve_contract_forwards(fs, project_root, &mut files);
    let method_agreement = if whole_project {
        resolve_method_agreement(&files)
    } else {
        HashMap::new()
    };
    let overridden = resolve_overridden_methods(&files);
    resolve_transitive_helper_summaries(fs, project_root, &mut files);
    let governed = resolve_param_governed_call_sites(fs, project_root, &files);
    let called_functions = resolve_called_functions(fs, project_root, &files);
    // Served their purpose: the index that goes on to the checks does not carry them.
    for entry in files.values_mut() {
        entry.call_sites = CallSites::default();
        entry.class_method_names = HashMap::new();
        entry.protocol_classes = Vec::new();
    }
    let call_usage = if with_callers {
        resolve_call_usage(fs, &files)
    } else {
        HashMap::new()
    };
    ProjectIndex {
        version: 1,
        files,
        all_schemas,
        all_schema_locations,
        call_site_errors: governed.call_site_errors,
        resolved_governed: governed.resolved_governed,
        called_functions,
        call_usage,
        method_agreement,
        overridden,
        exports_memo: ExportsMemo::default(),
        package_roots: crate::config::PackageRoots::default(),
    }
}

use super::*;

// How many import hops past the checked file `build_single_file_index_internal` will
// follow. Depth 1 is the file's own direct imports; depth 2 is what those files
// themselves import, and so on.
//
// Three is the smallest bound that covers the shapes this checker actually needs, with
// one hop of slack:
//   - depth 1: `pipeline.py` imports a schema straight from `schemas.py`.
//   - depth 2: `pipeline.py` imports `load_users` from `loaders.py`, whose return
//     annotation names a schema that lives in a THIRD file, `schemas.py` — the case
//     `all_schemas` exists for (see `compute_all_schemas`), and by far the most common
//     real layout.
//   - depth 3: one more hop for a delegate chain (`resolve_transitive_requires`), where
//     the imported function forwards its DataFrame on to a function in a further file
//     that is the one actually accessing columns.
// Past that, the marginal chance of the extra file mattering to THIS file's diagnostics
// drops off sharply while the fan-out keeps multiplying, so the bound is a deliberate
// "conservative, no surprising slowness" cut-off rather than a technical limit — the
// whole point of single-file mode is staying proportional to what the checked file
// actually references. A project that wants unbounded resolution already has it:
// checking the directory.
pub(crate) const MAX_SINGLE_FILE_IMPORT_DEPTH: usize = 3;

// Hard ceiling on how many first-party files the single-file import walk will index,
// regardless of depth. Bounds the pathological case the depth limit alone doesn't: a
// file importing a wide module which itself imports another wide module, where each
// hop multiplies rather than adds. 200 is comfortably above any realistic import
// closure at depth 3 (the shapes above reach 2-5 files) while still being a small
// fraction of a large project's file count, so hitting it means the walk had already
// stopped resembling "this file's own references" — the same spirit as
// MAX_AUTO_TRACED_PACKAGES here and MAX_SQL_FILE_READS_PER_FILE in linter.rs.
pub(crate) const MAX_SINGLE_FILE_INDEXED_FILES: usize = 200;

// Resolve a dotted module name to a first-party file ON DISK, without needing an
// already-populated index the way `resolve_module_file` does. Used only by the
// single-file import walk, which is discovering the files to index in the first place
// and so has nothing to look them up in yet.
//
// Deliberately tries exactly the same two roots `resolve_module_file` does — the
// project root and a `src/` layout — so that every file this walk decides to index is
// also a file check-time resolution (`Linter::load_cross_file_symbols`) can actually
// find again. A package `__init__.py` is NOT a candidate for the same reason: check
// time wouldn't resolve `from pkg import X` to it either (in whole-project mode just
// as much as here), so indexing it would add cost for a symbol nothing would ever look
// up. A resolved path under an excluded directory is dropped, matching what
// `collect_py_files` would have pruned in whole-project mode.
pub(crate) fn resolve_first_party_module_path(
    fs: &dyn crate::ports::FileSystem,
    module_name: &str,
    project_root: &Path,
    excluded_dirs: &std::collections::HashSet<String>,
) -> Option<PathBuf> {
    let mod_path = module_name.replace('.', "/");
    let candidate = [
        project_root.join(format!("{mod_path}.py")),
        project_root.join("src").join(format!("{mod_path}.py")),
        project_root.join(&mod_path).join("__init__.py"),
        project_root.join("src").join(&mod_path).join("__init__.py"),
    ]
    .into_iter()
    .find(|p| fs.is_file(p))?;
    let under_excluded_dir = candidate
        .strip_prefix(project_root)
        .ok()?
        .components()
        .any(|c| excluded_dirs.contains(c.as_os_str().to_string_lossy().as_ref()));
    (!under_excluded_dir).then_some(candidate)
}

// The file `from <dots><name> import ...` / `import <name>` in `importer` loads: relative to
// the importer, or under the project root, else under the importer's own `package_root`.
pub(super) fn resolve_import_path(
    fs: &dyn crate::ports::FileSystem,
    level: u32,
    name: &str,
    importer: &Path,
    project_root: &Path,
    excluded_dirs: &std::collections::HashSet<String>,
) -> Option<PathBuf> {
    if level == 0 {
        if name.is_empty() {
            return None;
        }
        return resolve_first_party_module_path(fs, name, project_root, excluded_dirs).or_else(
            || {
                let own = crate::config::package_root(fs, importer);
                (own != project_root)
                    .then(|| resolve_first_party_module_path(fs, name, &own, excluded_dirs))
                    .flatten()
            },
        );
    }
    let mut dir = importer.parent()?;
    for _ in 1..level {
        dir = dir.parent()?;
    }
    let base = if name.is_empty() {
        dir.to_path_buf()
    } else {
        dir.join(name.replace('.', "/"))
    };
    [base.with_extension("py"), base.join("__init__.py")]
        .into_iter()
        .find(|p| fs.is_file(p))
}

// Build a ProjectIndex scoped to ONE file: the file itself, the project-local modules
// its own `import` / `from ... import ...` statements reach (transitively, bounded by
// MAX_SINGLE_FILE_IMPORT_DEPTH and MAX_SINGLE_FILE_INDEXED_FILES), and any external
// installed package that closure calls in a DataFrame-shaped way.
//
// This is the single-file counterpart to `build_index_internal`, and the difference is
// entirely in how the file set is chosen: whole-project mode walks every `.py` file
// under the root (`collect_py_files`) because it has to produce an index valid for
// every file it will then check; here there is exactly one file to check, so the walk
// follows that file's own import graph instead and never lists a directory it wasn't
// pointed at. Both phases after that — external-package tracing and
// `finalise_index`'s cross-file resolution — are shared verbatim, since both are folds
// over whatever file set they're handed.
//
// Without a `pyproject.toml` above the file, imports resolve from its `package_root`.
#[cfg(test)]
pub(crate) fn build_single_file_index_internal(file_path: &Path) -> Option<ProjectIndex> {
    build_single_file_index_cached(&crate::ports::test_files::OsFiles, file_path, None)
}

// Builds the index for one file and its imports, using the per-file `cache` when given.
pub fn build_single_file_index_cached(
    fs: &dyn crate::ports::FileSystem,
    file_path: &Path,
    cache: Option<&dyn crate::ports::IndexCache>,
) -> Option<ProjectIndex> {
    let project_root = crate::config::import_root(fs, file_path);
    let config = load_linter_config(fs, &project_root);
    let indexer = FileIndexer::new(fs, &project_root, &config, cache);
    let excluded_dirs = resolve_excluded_dirs(&config);

    let mut files: HashMap<String, IndexEntry> = HashMap::new();
    let mut discovered_candidates: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    // Breadth-first, so the closest (and most likely relevant) files are the ones that
    // get indexed if MAX_SINGLE_FILE_INDEXED_FILES cuts the walk short.
    let mut queue: std::collections::VecDeque<(PathBuf, usize)> =
        std::collections::VecDeque::from([(file_path.to_path_buf(), 0)]);
    let mut seen: std::collections::HashSet<PathBuf> =
        std::collections::HashSet::from([file_path.to_path_buf()]);

    while let Some((current, depth)) = queue.pop_front() {
        if files.len() >= MAX_SINGLE_FILE_INDEXED_FILES {
            break;
        }
        let Some((entry, candidates)) = indexer.index(&current) else {
            continue;
        };
        discovered_candidates.extend(candidates);
        if depth < MAX_SINGLE_FILE_IMPORT_DEPTH {
            for (level, module_name) in &entry.import_targets {
                let Some(target) = resolve_import_path(
                    fs,
                    *level,
                    module_name,
                    &current,
                    &project_root,
                    &excluded_dirs,
                ) else {
                    continue;
                };
                if seen.insert(target.clone()) {
                    queue.push_back((target, depth + 1));
                }
            }
        }
        if let Some(path_str) = current.to_str() {
            files.insert(path_str.to_string(), entry);
        }
    }

    index_traced_external_packages(
        fs,
        &project_root,
        &config,
        discovered_candidates,
        &mut files,
        &indexer,
    );
    indexer.save(false);
    Some(finalise_index(fs, &project_root, files, false, false))
}

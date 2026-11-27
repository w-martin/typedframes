use super::*;

// Resolve each function's `param_schema_name` (the schema its first parameter is
// annotated with, e.g. `Annotated[pl.DataFrame, ReportSchema]`) to that schema's
// actual column list, overriding the heuristic body-scan `requires` with the
// authoritative one. Must run project-wide, after every file is indexed: the schema
// may be defined in a THIRD file — neither the one containing the function nor the
// one importing it — so a single file's own Linter pass (see visit_stmt) often can't
// resolve it locally. Runs before resolve_transitive_requires so that functions
// delegating to a schema-annotated one inherit the authoritative set, not the
// heuristic fallback.
pub(crate) fn resolve_param_schema_requires(
    files: &mut HashMap<String, IndexEntry>,
    all_schemas: &HashMap<String, Vec<String>>,
) {
    for entry in files.values_mut() {
        for func in entry.functions.values_mut() {
            if func.param_schema_name.is_empty() {
                continue;
            }
            if let Some(cols) = all_schemas.get(&func.param_schema_name) {
                let mut cols = cols.clone();
                cols.sort();
                cols.dedup();
                func.requires = cols;
                func.straight = true;
            }
        }
    }
}

// A function identified by the file that defines it and its name within that file.
pub(crate) type FuncNode = (String, String);

// Resolve a delegate call target (a bare name, as written at the call site) to the
// node that actually defines it: a same-file function first, else a function
// reached by following `from_file`'s own `from X import name` statements.
// Resolve a dotted module name (e.g. "pkg.transforms") to the project file that
// defines it, trying both the project root and a `src/` layout — same candidate
// paths used throughout this module for cross-file resolution.
pub(crate) fn resolve_module_file(
    fs: &dyn crate::ports::FileSystem,
    module_name: &str,
    project_root: &Path,
    files: &HashMap<String, IndexEntry>,
) -> Option<String> {
    let mod_path = module_name.replace('.', "/");
    let mut candidates = vec![
        project_root.join(format!("{mod_path}.py")),
        project_root.join("src").join(format!("{mod_path}.py")),
        project_root.join(&mod_path).join("__init__.py"),
        project_root.join("src").join(&mod_path).join("__init__.py"),
    ];
    // An explicitly allowlisted external package (see collect_external_package_files)
    // is indexed into the same `files` map under its real site-packages path — these
    // two extra candidates are how an import of it actually gets found.
    if let Some(site_packages) = find_site_packages_dir(fs, project_root) {
        candidates.push(site_packages.join(format!("{mod_path}.py")));
        candidates.push(site_packages.join(&mod_path).join("__init__.py"));
    }
    candidates
        .iter()
        .filter_map(|p| p.to_str())
        .find(|p| files.contains_key(*p))
        .map(str::to_string)
}

// `resolve_module_file`, falling back to the importer's own `package_root` when the
// project root (a `pyproject.toml` above code that is not on its import path) misses. The
// importer's root is walked once per `own_root` and once per directory in `roots`.
pub(crate) fn resolve_module_file_with_root(
    fs: &dyn crate::ports::FileSystem,
    module_name: &str,
    project_root: &Path,
    importer: Option<&Path>,
    own_root: &std::cell::OnceCell<PathBuf>,
    roots: &crate::config::PackageRoots,
    files: &HashMap<String, IndexEntry>,
) -> Option<String> {
    resolve_module_file(fs, module_name, project_root, files).or_else(|| {
        let importer = importer?;
        let own = own_root.get_or_init(|| roots.of_file(fs, importer));
        (own != project_root)
            .then(|| resolve_module_file(fs, module_name, own, files))
            .flatten()
    })
}

// Resolve a delegate call target (a bare name, as written at the call site) to the
// node that actually defines it: a same-file function first, else a function
// reached by following `from_file`'s own `from X import name` statements, else —
// since attribute-style calls (`transforms.enrich(df)`) are recorded as the bare
// attribute name `enrich` with no module prefix (see scan_expr_for_contract) — a
// function by that name in any module `from_file` plainly `import`s. That last
// step can't distinguish which plainly-imported module a given call actually used
// if more than one defines a same-named function; same conservative trade-off as
// the rest of this heuristic (see the cycle-handling note on resolve_node_requires).
pub(crate) fn resolve_delegate_target(
    fs: &dyn crate::ports::FileSystem,
    from_file: &str,
    callee: &str,
    project_root: &Path,
    files: &HashMap<String, IndexEntry>,
) -> Option<FuncNode> {
    let entry = files.get(from_file)?;
    if entry.functions.contains_key(callee) {
        return Some((from_file.to_string(), callee.to_string()));
    }
    if let Some(module_name) = entry.imports.get(callee) {
        let target_file = resolve_module_file(fs, module_name, project_root, files)?;
        if files.get(&target_file)?.functions.contains_key(callee) {
            return Some((target_file, callee.to_string()));
        }
    }
    for module_name in entry.module_aliases.values() {
        let Some(target_file) = resolve_module_file(fs, module_name, project_root, files) else {
            continue;
        };
        if files.get(&target_file)?.functions.contains_key(callee) {
            return Some((target_file, callee.to_string()));
        }
    }
    None
}

// A `module.callee` delegate's target: `module` resolved through `from_file`'s own plain
// `import` aliases (never `from X import name`, which delegate carries as a bare name
// instead -- see `resolve_delegate_target`), then `callee` looked up in that module's file.
pub(super) fn resolve_module_call_target(
    fs: &dyn crate::ports::FileSystem,
    from_file: &str,
    module: &str,
    callee: &str,
    project_root: &Path,
    files: &HashMap<String, IndexEntry>,
) -> Option<FuncNode> {
    let entry = files.get(from_file)?;
    let module_name = entry.module_aliases.get(module)?;
    let target_file = resolve_module_file(fs, module_name, project_root, files)?;
    files
        .get(&target_file)?
        .functions
        .contains_key(callee)
        .then(|| (target_file, callee.to_string()))
}

// Project-wide pass: for every direct `x = name(...)` call site anywhere in the
// project, resolve the callee to its defining (file, func) via `resolve_delegate_target`
// and record it unconditionally -- see `ProjectIndex.called_functions`'s doc comment
// for what this is for. Much simpler than `resolve_param_governed_call_sites`/
// `check_governed_call_site`: no template or literal-value resolution, every call site
// that resolves at all is recorded.
pub(crate) fn resolve_called_functions(
    fs: &dyn crate::ports::FileSystem,
    project_root: &Path,
    files: &HashMap<String, IndexEntry>,
) -> HashMap<String, std::collections::HashSet<String>> {
    let mut called: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
    let has_any_functions = files
        .values()
        .any(|entry| !entry.functions.is_empty() || !entry.class_methods.is_empty());
    if !has_any_functions {
        return called;
    }

    // Each file's recorded call sites only read the shared index, so files are resolved in
    // parallel and their results merged afterwards.
    let entries: Vec<(&String, &IndexEntry)> = files.iter().collect();
    let per_file = crate::parallel::parallel_map(&entries, |(file_path, entry)| {
        let mut local: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
        resolve_call_sites(
            fs,
            file_path,
            project_root,
            files,
            &entry.call_sites,
            &mut local,
        );
        local
    });
    for local in per_file {
        for (target, names) in local {
            called.entry(target).or_default().extend(names);
        }
    }
    called
}

// Project-wide merge of what each file shows about the calls to every name (see
// `call_usage`): a name is kept only when no file hides a use of it, at least one call
// reads columns, and the project defines a function of that name. A file that can't be read
// or parsed may hold calls this can't see, so then nothing is kept.
pub(crate) fn resolve_call_usage(
    fs: &dyn crate::ports::FileSystem,
    files: &HashMap<String, IndexEntry>,
) -> HashMap<String, CallUsage> {
    let mut modules = Vec::with_capacity(files.len());
    let mut defined: std::collections::HashSet<String> = std::collections::HashSet::new();
    for file_path in files.keys() {
        let Ok(source) = fs.read_to_string(Path::new(file_path)) else {
            return HashMap::new();
        };
        let Ok(parsed) = parse_module(&source) else {
            return HashMap::new();
        };
        let module = parsed.into_syntax();
        defined.extend(crate::call_usage::defined_names(&module));
        modules.push(module);
    }
    let mut merged: HashMap<String, crate::call_usage::Observation> = HashMap::new();
    for module in &modules {
        for (name, seen) in crate::call_usage::module_call_usage(module, &defined).observations {
            let total = merged.entry(name).or_default();
            total.calls += seen.calls;
            total.unknown |= seen.unknown;
            total.columns.extend(seen.columns);
        }
    }
    merged
        .into_iter()
        .filter(|(name, seen)| {
            !seen.unknown && seen.calls > 0 && !seen.columns.is_empty() && defined.contains(name)
        })
        .map(|(name, seen)| {
            (
                name,
                CallUsage {
                    columns: seen.columns.into_iter().collect(),
                    calls: seen.calls,
                },
            )
        })
        .collect()
}

// Resolve a `self.<attr>` origin (see `collect_self_attr_origins`) to the file that
// defines its class: same-file first (a class defined in `from_file` itself), then
// `imports`/`module_aliases` -- mirroring how a real `Linter`'s `self.class_methods`
// ends up covering both cases (local class visits plus `import_name`'s cross-file
// pull), just resolved fresh here since this lightweight re-scan has no `Linter`
// instance whose state already merged them (see `index_file`, which never calls
// `load_cross_file_symbols` before its own walk).
pub(super) fn resolve_self_attr_class_file(
    fs: &dyn crate::ports::FileSystem,
    from_file: &str,
    origin: &SelfAttrOrigin,
    project_root: &Path,
    files: &HashMap<String, IndexEntry>,
) -> Option<String> {
    let from_entry = files.get(from_file)?;
    if from_entry.class_methods.contains_key(&origin.class_name) {
        return Some(from_file.to_string());
    }
    let module_name = if origin.is_attribute_call {
        from_entry.module_aliases.get(&origin.resolve_name)
    } else {
        from_entry.imports.get(&origin.resolve_name)
    }?;
    let target_file = resolve_module_file(fs, module_name, project_root, files)?;
    if files
        .get(&target_file)?
        .class_methods
        .contains_key(&origin.class_name)
    {
        Some(target_file)
    } else {
        None
    }
}

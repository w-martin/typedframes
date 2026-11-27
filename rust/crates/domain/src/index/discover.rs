use super::*;

// Recursively collect all `.py` files under `dir`, skipping any name in
// `resolved_excludes` -- the caller's job to resolve (see `build_index_internal`):
// either `[tool.typedframes] exclude`'s configured list, replacing
// `DEFAULT_EXCLUDED_DIRS` entirely, or `DEFAULT_EXCLUDED_DIRS` itself when nothing is
// configured. Uses an explicit stack rather than recursion to avoid stack overflow on
// very deep trees.
pub(crate) fn collect_py_files(
    fs: &dyn crate::ports::FileSystem,
    dir: &Path,
    resolved_excludes: &std::collections::HashSet<String>,
) -> Vec<PathBuf> {
    let mut result = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = fs.read_dir(&current) else {
            continue;
        };
        for entry in entries {
            let path = entry.path;
            let name = path.file_name().unwrap_or_default();
            let name_str = name.to_string_lossy();
            if resolved_excludes.contains(name_str.as_ref()) {
                continue;
            }
            if entry.is_dir {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("py") {
                result.push(path);
            }
        }
    }
    result
}

// Auto-detect a project's virtualenv site-packages directory, for resolving
// explicitly allowlisted external packages (`trace_external_packages` config option).
// Tries the conventional `.venv` layout on both Unix (`lib/pythonX.Y/site-packages`,
// version-globbed since it depends on the interpreter the venv was created with) and
// Windows (`Lib/site-packages`). No path override in this version — this is the
// convention this project's own examples use (a `uv`-managed `.venv`); a project using
// a differently-named or externally-managed environment simply won't resolve anything,
// same as any other candidate path that doesn't exist.
//
// Found once per thread and root, for the thread's lifetime: every import resolution asks, and
// a large project would otherwise `read_dir` the same `lib/` directory once per import
// statement. Per thread rather than shared, because a lock that every worker reads still makes
// them queue on one cache line.
thread_local! {
    static SITE_PACKAGES_MEMO: std::cell::RefCell<HashMap<PathBuf, Option<PathBuf>>> =
        std::cell::RefCell::new(HashMap::new());
}

pub(crate) fn find_site_packages_dir(
    fs: &dyn crate::ports::FileSystem,
    project_root: &Path,
) -> Option<PathBuf> {
    SITE_PACKAGES_MEMO.with(|memo| {
        if let Some(found) = memo.borrow().get(project_root) {
            return found.clone();
        }
        let found = find_site_packages_dir_uncached(fs, project_root);
        memo.borrow_mut()
            .insert(project_root.to_path_buf(), found.clone());
        found
    })
}

pub(crate) fn find_site_packages_dir_uncached(
    fs: &dyn crate::ports::FileSystem,
    project_root: &Path,
) -> Option<PathBuf> {
    let venv = project_root.join(".venv");
    if !fs.is_dir(&venv) {
        return None;
    }
    let windows_layout = venv.join("Lib").join("site-packages");
    if fs.is_dir(&windows_layout) {
        return Some(windows_layout);
    }
    let lib_dir = venv.join("lib");
    let entries = fs.read_dir(&lib_dir).ok()?;
    for entry in entries {
        let path = entry.path;
        if !fs.is_dir(&path) {
            continue;
        }
        if !path
            .file_name()
            .map(|n| n.to_string_lossy().starts_with("python"))
            .unwrap_or(false)
        {
            continue;
        }
        let candidate = path.join("site-packages");
        if fs.is_dir(&candidate) {
            return Some(candidate);
        }
    }
    None
}

// Whether an external package declares PEP 561 `py.typed` -- a plain marker file at
// the package's own root (`<site-packages>/<package>/py.typed`), meaning its author
// intends its inline (or bundled `.pyi`) type annotations to be trusted by tooling.
// Used to let a self.<attr>.<method>(...) call resolve immediately against that
// package's actual return-type annotations (once traced -- see index_file's
// `all_self_attr_origins` handling below), rather than needing
// `dataframe_shaped_usage`'s "was the result actually used like a DataFrame"
// confirmation first, the way plain auto-discovery still does for packages that don't
// make this promise. Absence isn't a judgment on the package -- it just means this
// checker has no first-party signal that its annotations (if any) are meant to be
// consumed, so it falls back to behavioral discovery like everything else.
pub(crate) fn package_declares_py_typed(
    fs: &dyn crate::ports::FileSystem,
    project_root: &Path,
    package: &str,
) -> bool {
    let Some(site_packages) = find_site_packages_dir(fs, project_root) else {
        return false;
    };
    fs.is_file(&site_packages.join(package).join("py.typed"))
}

// Collect `.py` files for each named external package, resolved from the project's
// own site-packages directory. `packages` is the already-merged, already-filtered set
// -- see `resolve_traced_external_packages` for how `trace_external_packages`
// (explicit force-include), auto-discovered candidates, and `excluded_external_packages`
// combine into it. Deliberately narrow: only the named packages' own directories are
// walked — never the whole site-packages tree, which would be both expensive and a far
// larger, unbounded trust surface than a bounded, explainable package list.
pub(crate) fn collect_external_package_files(
    fs: &dyn crate::ports::FileSystem,
    project_root: &Path,
    packages: &[String],
) -> Vec<PathBuf> {
    if packages.is_empty() {
        return Vec::new();
    }
    let Some(site_packages) = find_site_packages_dir(fs, project_root) else {
        return Vec::new();
    };
    let mut result = Vec::new();
    for package in packages {
        let package_dir = site_packages.join(package);
        if fs.is_dir(&package_dir) {
            // Never excludes anything here -- `exclude` is about pruning irrelevant
            // project-local directories, not the internal structure of a package the
            // user has explicitly opted into tracing.
            result.extend(collect_py_files(
                fs,
                &package_dir,
                &std::collections::HashSet::new(),
            ));
            continue;
        }
        let single_module = site_packages.join(format!("{package}.py"));
        if fs.is_file(&single_module) {
            result.push(single_module);
        }
    }
    result
}

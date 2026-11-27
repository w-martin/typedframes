use super::*;

// Every project module `module` imports that exports statically known constants or
// functions, keyed as written (`(leading dots, dotted name)`). `from a import b` also tries
// `a.b`, in case `b` is a submodule.
pub(super) fn collect_imported_modules(
    fs: &dyn crate::ports::FileSystem,
    index: &ProjectIndex,
    module: &ModModule,
    project_root: &Path,
    file_path: Option<&str>,
) -> static_eval::ImportedModules {
    let mut imports = Imports(Vec::new());
    for stmt in &module.body {
        imports.visit_stmt(stmt);
    }
    let importer = file_path.map(|p| {
        let p = Path::new(p);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            project_root.join(p)
        }
    });
    let mut found = static_eval::ImportedModules::new();
    let own_root = std::cell::OnceCell::new();
    for (level, name) in imports.0 {
        if found.contains_key(&(level, name.clone())) {
            continue;
        }
        let target = resolve_import(
            fs,
            index,
            level,
            &name,
            importer.as_deref(),
            &own_root,
            project_root,
        );
        if let Some(exports) = target.and_then(|t| module_exports(fs, index, &t, project_root, 0)) {
            if !exports.is_empty() {
                found.insert((level, name), exports);
            }
        }
    }
    found
}

struct Imports(Vec<(u32, String)>);

impl ast_visitor::Visitor<'_> for Imports {
    fn visit_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Import(i) => {
                for alias in &i.names {
                    self.0.push((0, alias.name.to_string()));
                }
            }
            Stmt::ImportFrom(i) => {
                let name = i.module.as_ref().map(|m| m.to_string()).unwrap_or_default();
                for alias in &i.names {
                    if alias.name.as_str() != "*" {
                        self.0.push((
                            i.level,
                            crate::index::join_module(&name, alias.name.as_str()),
                        ));
                    }
                }
                self.0.push((i.level, name));
            }
            _ => {}
        }
        ast_visitor::walk_stmt(self, stmt);
    }
}

// The indexed file `from <dots><name> import ...` in `importer` (or a notebook) loads.
fn resolve_import(
    fs: &dyn crate::ports::FileSystem,
    index: &ProjectIndex,
    level: u32,
    name: &str,
    importer: Option<&Path>,
    own_root: &std::cell::OnceCell<PathBuf>,
    project_root: &Path,
) -> Option<String> {
    if level == 0 {
        if name.is_empty() {
            return None;
        }
        resolve_module_file_with_root(
            fs,
            name,
            project_root,
            importer,
            own_root,
            &index.package_roots,
            &index.files,
        )
    } else {
        resolve_relative_module(importer?, level, name, &index.files)
    }
}

// What `file` exports to an importer, with the names it re-exports (`from .cols import Col`
// in a package `__init__.py`) resolved to their values in the modules they come from.
fn module_exports(
    fs: &dyn crate::ports::FileSystem,
    index: &ProjectIndex,
    file: &str,
    project_root: &Path,
    depth: usize,
) -> Option<Arc<static_eval::ModuleExports>> {
    let key = (file.to_string(), depth, project_root.to_path_buf());
    if let Some(done) = index.exports_memo.get(&key) {
        return done;
    }
    let result = compute_module_exports(fs, index, file, project_root, depth).map(Arc::new);
    index.exports_memo.insert(key, result.clone());
    result
}

fn compute_module_exports(
    fs: &dyn crate::ports::FileSystem,
    index: &ProjectIndex,
    file: &str,
    project_root: &Path,
    depth: usize,
) -> Option<static_eval::ModuleExports> {
    let entry = index.files.get(file)?;
    let mut exports = entry.eval.clone();
    if depth >= MAX_REEXPORT_DEPTH {
        return Some(exports);
    }
    let own_root = std::cell::OnceCell::new();
    for (local, (level, module, name)) in &entry.eval.reexports {
        let Some(source) = resolve_import(
            fs,
            index,
            *level,
            module,
            Some(Path::new(file)),
            &own_root,
            project_root,
        )
        .and_then(|t| module_exports(fs, index, &t, project_root, depth + 1)) else {
            continue;
        };
        if let Some(value) = source.constants.get(name) {
            exports
                .constants
                .entry(local.clone())
                .or_insert_with(|| value.clone());
        }
        let prefix = format!("{name}.");
        for (key, value) in &source.constants {
            if let Some(attr) = key.strip_prefix(&prefix) {
                exports
                    .constants
                    .entry(format!("{local}.{attr}"))
                    .or_insert_with(|| value.clone());
            }
        }
        if let Some(members) = source.enums.get(name) {
            exports
                .enums
                .entry(local.clone())
                .or_insert_with(|| members.clone());
        }
    }
    Some(exports)
}

const MAX_REEXPORT_DEPTH: usize = 4;

pub(super) fn resolve_relative_module(
    importer: &Path,
    level: u32,
    name: &str,
    files: &HashMap<String, IndexEntry>,
) -> Option<String> {
    let mut dir = importer.parent()?;
    for _ in 1..level {
        dir = dir.parent()?;
    }
    let base = if name.is_empty() {
        dir.to_path_buf()
    } else {
        dir.join(name.replace('.', "/"))
    };
    let candidates = [base.with_extension("py"), base.join("__init__.py")];
    candidates
        .iter()
        .filter_map(|p| p.to_str())
        .find(|p| files.contains_key(*p))
        .map(str::to_string)
}

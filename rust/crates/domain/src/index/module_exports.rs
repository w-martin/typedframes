use super::*;

// Parse one `.py` file and extract its symbols into an IndexEntry.
// Runs the linter in index mode (diagnostics discarded) to collect schemas and
// functions, then separately parses `__all__` assignments and `from X import Y`
// statements for wildcard-import support and delegate-target resolution respectively.
// Returns the file's `IndexEntry` plus the top-level package names of any external
// (non-first-party) imports it called in an unresolved, DataFrame-shaped way (see
// `Linter::unresolved_dataframe_shaped_calls`) -- candidates for on-demand external
// package tracing, resolved by `discover_external_package_candidates` in
// `build_index_internal`. This file's own imports/module_aliases are exactly what's
// needed to resolve each candidate's raw call-site name to a module, so it's done
// here rather than requiring a second pass over the same file.
// The names a module exports through `__all__`, the names it imports with `from X import Y`
// (each to the module it came from), and its plain `import module [as alias]` aliases.
pub(super) fn module_exports_and_imports(
    module: &ruff_python_ast::ModModule,
) -> (
    Vec<String>,
    HashMap<String, String>,
    HashMap<String, String>,
) {
    let mut exports = Vec::new();
    let mut imports: HashMap<String, String> = HashMap::new();
    let mut module_aliases: HashMap<String, String> = HashMap::new();
    for stmt in crate::linter::top_level_and_type_checking_stmts(module) {
        collect_module_statement(&mut exports, &mut imports, &mut module_aliases, stmt);
    }
    (exports, imports, module_aliases)
}

fn collect_module_statement(
    exports: &mut Vec<String>,
    imports: &mut HashMap<String, String>,
    module_aliases: &mut HashMap<String, String>,
    stmt: &Stmt,
) {
    match stmt {
        Stmt::Assign(assign) => {
            for target in &assign.targets {
                let Expr::Name(name) = target else {
                    continue;
                };
                if name.id.as_str() != "__all__" {
                    continue;
                }
                let Expr::List(list) = &*assign.value else {
                    continue;
                };
                for el in &list.elts {
                    if let Expr::StringLiteral(s) = el {
                        exports.push(s.value.to_str().to_string());
                    }
                }
            }
        }
        Stmt::ImportFrom(import_from) if import_from.level == 0 => {
            if let Some(module_ident) = &import_from.module {
                let module_name = module_ident.id.to_string();
                for alias in &import_from.names {
                    imports.insert(alias.name.id.to_string(), module_name.clone());
                }
            }
        }
        // Plain `import module [as alias]` / `import a.b.c [as alias]`. Without
        // an alias, Python binds the FIRST segment of a dotted import (`import
        // a.b.c` binds `a`), so a dotted single-segment module (the common case
        // for a flat project: `import transforms`) binds cleanly; a genuinely
        // nested `a.b.c` accessed as `a.b.c.func()` without an alias is not
        // tracked here (`a.func()` wouldn't resolve to `a.b.c` anyway) — same
        // conservative-not-exhaustive trade-off as the rest of this heuristic.
        Stmt::Import(import_stmt) => {
            for alias in &import_stmt.names {
                let dotted = alias.name.id.to_string();
                let local_name = match &alias.asname {
                    Some(asname) => asname.id.to_string(),
                    None => dotted.split('.').next().unwrap_or(&dotted).to_string(),
                };
                module_aliases.insert(local_name, dotted);
            }
        }
        _ => {}
    }
}

pub(super) fn import_targets(module: &ruff_python_ast::ModModule) -> Vec<(u32, String)> {
    let mut targets = Vec::new();
    for stmt in crate::linter::top_level_and_type_checking_stmts(module) {
        match stmt {
            Stmt::Import(import) => {
                targets.extend(import.names.iter().map(|a| (0, a.name.to_string())));
            }
            Stmt::ImportFrom(import) => {
                let module = import
                    .module
                    .as_ref()
                    .map(|m| m.to_string())
                    .unwrap_or_default();
                for alias in &import.names {
                    if alias.name.as_str() != "*" {
                        targets.push((import.level, join_module(&module, alias.name.as_str())));
                    }
                }
                targets.push((import.level, module));
            }
            _ => {}
        }
    }
    targets
}

/// `package.name`, or `name` alone for a bare relative import (`from . import name`).
pub(crate) fn join_module(package: &str, name: &str) -> String {
    if package.is_empty() {
        name.to_string()
    } else {
        format!("{package}.{name}")
    }
}

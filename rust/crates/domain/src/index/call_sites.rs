use super::*;

// Records the statement-level calls in `stmts`: every callee name, and each
// `self.<attr>.<method>(...)` whose `<attr>` is one of the enclosing class's `__init__`
// origins. The traversal is the one `resolve_called_functions` has always used -- function and
// class bodies are entered, a class's own `__init__` origins replace any enclosing class's.
fn collect_call_sites(
    stmts: &[Stmt],
    self_attrs: Option<&HashMap<String, SelfAttrOrigin>>,
    names: &mut std::collections::BTreeSet<String>,
    self_calls: &mut Vec<(SelfAttrOrigin, String)>,
) {
    for stmt in stmts {
        collect_statement_call(self_attrs, names, self_calls, stmt);
    }
}

fn collect_statement_call(
    self_attrs: Option<&HashMap<String, SelfAttrOrigin>>,
    names: &mut std::collections::BTreeSet<String>,
    self_calls: &mut Vec<(SelfAttrOrigin, String)>,
    stmt: &Stmt,
) {
    let call = match stmt {
        Stmt::Expr(expr_stmt) => match &*expr_stmt.value {
            Expr::Call(c) => Some(c),
            _ => None,
        },
        Stmt::Assign(assign) => match &*assign.value {
            Expr::Call(c) => Some(c),
            _ => None,
        },
        _ => None,
    };
    if let Some(call) = call {
        collect_call_sites_call(self_attrs, names, self_calls, call);
    }
    collect_nested_call_sites(self_attrs, names, self_calls, stmt);
}

fn collect_call_sites_call(
    self_attrs: Option<&HashMap<String, SelfAttrOrigin>>,
    names: &mut std::collections::BTreeSet<String>,
    self_calls: &mut Vec<(SelfAttrOrigin, String)>,
    call: &ruff_python_ast::ExprCall,
) {
    let callee_name = match &*call.func {
        Expr::Name(n) => Some(n.id.as_str()),
        Expr::Attribute(attr) => Some(attr.attr.as_str()),
        _ => None,
    };
    if let Some(callee_name) = callee_name {
        names.insert(callee_name.to_string());
    }
    if let (Expr::Attribute(attr), Some(attrs)) = (&*call.func, self_attrs) {
        collect_self_attribute_call(self_calls, attr, attrs);
    }
}

fn collect_self_attribute_call(
    self_calls: &mut Vec<(SelfAttrOrigin, String)>,
    attr: &ruff_python_ast::ExprAttribute,
    attrs: &HashMap<String, SelfAttrOrigin>,
) {
    if let Expr::Attribute(recv_attr) = &*attr.value {
        collect_self_attribute_call_of(self_calls, attr, attrs, recv_attr);
    }
}

fn collect_self_attribute_call_of(
    self_calls: &mut Vec<(SelfAttrOrigin, String)>,
    attr: &ruff_python_ast::ExprAttribute,
    attrs: &HashMap<String, SelfAttrOrigin>,
    recv_attr: &ruff_python_ast::ExprAttribute,
) {
    if let Expr::Name(recv_base) = &*recv_attr.value {
        if recv_base.id.as_str() == "self" {
            if let Some(origin) = attrs.get(recv_attr.attr.as_str()) {
                let site = (origin.clone(), attr.attr.to_string());
                if !self_calls.contains(&site) {
                    self_calls.push(site);
                }
            }
        }
    }
}

fn collect_nested_call_sites(
    self_attrs: Option<&HashMap<String, SelfAttrOrigin>>,
    names: &mut std::collections::BTreeSet<String>,
    self_calls: &mut Vec<(SelfAttrOrigin, String)>,
    stmt: &Stmt,
) {
    match stmt {
        Stmt::If(if_stmt) => {
            collect_call_sites(&if_stmt.body, self_attrs, names, self_calls);
            for clause in &if_stmt.elif_else_clauses {
                collect_call_sites(&clause.body, self_attrs, names, self_calls);
            }
        }
        Stmt::For(for_stmt) => collect_call_sites(&for_stmt.body, self_attrs, names, self_calls),
        Stmt::While(while_stmt) => {
            collect_call_sites(&while_stmt.body, self_attrs, names, self_calls)
        }
        Stmt::With(with_stmt) => collect_call_sites(&with_stmt.body, self_attrs, names, self_calls),
        Stmt::Try(try_stmt) => {
            collect_call_sites(&try_stmt.body, self_attrs, names, self_calls);
            for handler in &try_stmt.handlers {
                let ast::ExceptHandler::ExceptHandler(h) = handler;
                collect_call_sites(&h.body, self_attrs, names, self_calls);
            }
            collect_call_sites(&try_stmt.orelse, self_attrs, names, self_calls);
            collect_call_sites(&try_stmt.finalbody, self_attrs, names, self_calls);
        }
        // Function and class bodies are entered: almost every real call site lives in a
        // function or method body. A nested function still sees the enclosing class's
        // `self.<attr>` origins, as a real Linter's `current_self_attrs` does.
        Stmt::FunctionDef(func_def) => {
            collect_call_sites(&func_def.body, self_attrs, names, self_calls)
        }
        Stmt::ClassDef(class_def) => {
            // Fresh per class, from THIS class's own `__init__` only -- a nested class
            // does not inherit an enclosing class's attributes.
            let mut class_self_attrs = HashMap::new();
            for body_stmt in &class_def.body {
                if let Stmt::FunctionDef(func_def) = body_stmt {
                    if func_def.name.as_str() == "__init__" {
                        collect_self_attr_origins(
                            &func_def.body,
                            &annotated_param_origins(func_def),
                            &mut class_self_attrs,
                        );
                    }
                }
            }
            collect_call_sites(&class_def.body, Some(&class_self_attrs), names, self_calls);
        }
        _ => {}
    }
}

/// The call sites of a module as written.
pub(crate) fn module_call_sites(module: &ruff_python_ast::ModModule) -> CallSites {
    let mut names = std::collections::BTreeSet::new();
    let mut self_calls = Vec::new();
    collect_call_sites(&module.body, None, &mut names, &mut self_calls);
    CallSites {
        names: names.into_iter().collect(),
        self_calls,
    }
}

// Resolves a file's recorded call sites to the functions and methods they call, recording
// each against its defining file.
pub(super) fn resolve_call_sites(
    fs: &dyn crate::ports::FileSystem,
    file_path: &str,
    project_root: &Path,
    files: &HashMap<String, IndexEntry>,
    sites: &CallSites,
    called: &mut HashMap<String, std::collections::HashSet<String>>,
) {
    for name in &sites.names {
        if let Some((target_file, target_func)) =
            resolve_delegate_target(fs, file_path, name, project_root, files)
        {
            called.entry(target_file).or_default().insert(target_func);
        }
    }
    // `self.<attr>.<method>(...)` is a shape `resolve_delegate_target` can't cover (it only
    // resolves bare names through imports and module aliases): it goes through the class the
    // attribute was assigned.
    for (origin, method) in &sites.self_calls {
        let Some(target_file) =
            resolve_self_attr_class_file(fs, file_path, origin, project_root, files)
        else {
            continue;
        };
        let method_known = files
            .get(&target_file)
            .and_then(|e| e.class_methods.get(&origin.class_name))
            .is_some_and(|methods| methods.contains_key(method.as_str()));
        if method_known {
            called
                .entry(target_file)
                .or_default()
                .insert(method.clone());
        }
    }
}

// Line/column for a source offset, without needing a full `Linter` instance — used by
// resolve_param_governed_call_sites, which parses each file fresh outside any Linter's
// own `check_file_internal` pass.
pub(crate) fn compute_source_location(
    source: &str,
    offset: ruff_text_size::TextSize,
) -> (usize, usize) {
    let line_index = LineIndex::from_source_text(source);
    let source_code = SourceCode::new(source, &line_index);
    let loc = source_code.line_column(offset);
    (loc.line.get(), loc.column.get())
}

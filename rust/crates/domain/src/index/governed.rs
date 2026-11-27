use super::*;

// Project-wide pass: for every function that `find_param_governed_feast_template`
// found a template for, find every call site (anywhere in the project, at the same
// statement-nesting depth `analyze_stmt_for_contract` already covers) and check it
// against the template's recorded accesses — independently per call site, so two
// callers passing different arguments for the same parameter get diagnosed (or not)
// completely independently. A call site whose argument doesn't trace to a literal
// (a variable with no traceable origin, a dynamically-built list, ...) gets its own
// untracked-dataframe diagnostic right there, rather than falling back to the callee's
// generic one — the callee's own shape is exactly as resolvable as any other governed
// function; the ambiguity genuinely originates at the call site that couldn't produce
// a literal, so that's where the diagnostic belongs.
//
// Diagnostics are returned keyed by the *calling* file's path — not the callee's file
// — which is what lets this avoid ever needing two different diagnoses for the same
// source line: the callee's own access line is one, single, caller-independent AST
// location, and every different-per-caller outcome instead lives at that caller's own
// (necessarily distinct) call-site location.
pub(crate) struct GovernedCallSiteResult {
    pub(crate) call_site_errors: HashMap<String, Vec<LintError>>,
    pub(crate) resolved_governed: std::collections::HashSet<(String, String)>,
}

pub(crate) fn resolve_param_governed_call_sites(
    fs: &dyn crate::ports::FileSystem,
    project_root: &Path,
    files: &HashMap<String, IndexEntry>,
) -> GovernedCallSiteResult {
    let mut result = GovernedCallSiteResult {
        call_site_errors: HashMap::new(),
        resolved_governed: std::collections::HashSet::new(),
    };
    let has_any_governed = files
        .values()
        .any(|entry| entry.functions.values().any(|f| f.param_governed.is_some()));
    if !has_any_governed {
        return result;
    }

    for file_path in files.keys() {
        let Ok(source) = fs.read_to_string(Path::new(file_path)) else {
            continue;
        };
        let Ok(parsed) = parse_module(&source) else {
            continue;
        };
        let module = parsed.into_syntax();
        scan_stmts_for_governed_calls(
            fs,
            &module.body,
            file_path,
            &source,
            project_root,
            files,
            &mut result,
        );
    }
    result
}

pub(crate) fn scan_stmts_for_governed_calls(
    fs: &dyn crate::ports::FileSystem,
    stmts: &[Stmt],
    file_path: &str,
    source: &str,
    project_root: &Path,
    files: &HashMap<String, IndexEntry>,
    result: &mut GovernedCallSiteResult,
) {
    for stmt in stmts {
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
            check_governed_call_site(fs, call, file_path, source, project_root, files, result);
        }
        match stmt {
            Stmt::If(if_stmt) => {
                scan_stmts_for_governed_calls(
                    fs,
                    &if_stmt.body,
                    file_path,
                    source,
                    project_root,
                    files,
                    result,
                );
                for clause in &if_stmt.elif_else_clauses {
                    scan_stmts_for_governed_calls(
                        fs,
                        &clause.body,
                        file_path,
                        source,
                        project_root,
                        files,
                        result,
                    );
                }
            }
            Stmt::For(for_stmt) => {
                scan_stmts_for_governed_calls(
                    fs,
                    &for_stmt.body,
                    file_path,
                    source,
                    project_root,
                    files,
                    result,
                );
            }
            Stmt::While(while_stmt) => {
                scan_stmts_for_governed_calls(
                    fs,
                    &while_stmt.body,
                    file_path,
                    source,
                    project_root,
                    files,
                    result,
                );
            }
            Stmt::With(with_stmt) => {
                scan_stmts_for_governed_calls(
                    fs,
                    &with_stmt.body,
                    file_path,
                    source,
                    project_root,
                    files,
                    result,
                );
            }
            _ => {}
        }
    }
}

pub(crate) fn check_governed_call_site(
    fs: &dyn crate::ports::FileSystem,
    call: &ast::ExprCall,
    file_path: &str,
    source: &str,
    project_root: &Path,
    files: &HashMap<String, IndexEntry>,
    result: &mut GovernedCallSiteResult,
) {
    let Some((target_file, target_func, template)) =
        governed_target(fs, call, file_path, project_root, files)
    else {
        return;
    };
    let Some(arg_expr) = governing_argument(call, template) else {
        return; // not passed at this call site (e.g. a default value) -- nothing to check
    };
    // A general lazy evaluator: a direct literal list, a literal/f-string built from an
    // environment of substituted parameter values, or a call to another function --
    // whose own argument expressions are evaluated in the CURRENT (empty, at the call
    // site) environment, bound to the callee's parameters, and followed recursively
    // through its first return statement, as many hops as needed with cycle
    // protection. This is strictly more general than a zero-arg forward: a caller
    // doesn't have to pass the literal directly, or even forward a zero-arg call --
    // `helper("driver_stats")` is followed by substituting "driver_stats" for helper's
    // own parameter and evaluating its f-string return with it.
    let raw_items = eval_feast_list_expr(
        fs,
        arg_expr,
        &HashMap::new(),
        file_path,
        project_root,
        files,
        &mut std::collections::HashSet::new(),
    );
    let resolved_cols = raw_items.and_then(|raw| {
        ast_extract::feast_columns_from_raw_items(&raw, template.full_feature_names)
    });

    // This call site is real -- it passes SOMETHING for the governed parameter, whether
    // or not that something is traceable to a literal -- so the callee's own generic
    // "columns unknown at lint time" framing is stale either way: either this call site
    // resolves (validated right here, below), or it doesn't, in which case the SAME
    // untracked-dataframe signal is reported HERE instead of inside the callee, since
    // call-site tracing means this call site is where the real ambiguity actually
    // originates -- the callee's own shape is exactly as resolvable as any other
    // call-site-governed function, in the abstract. check_file consults this set either
    // way to retract the callee's own line.
    result
        .resolved_governed
        .insert((target_file.clone(), target_func.clone()));

    let at = compute_source_location(source, call.range().start());
    let errors = governed_call_errors(at, template, &target_file, &target_func, resolved_cols);
    if !errors.is_empty() {
        result
            .call_site_errors
            .entry(file_path.to_string())
            .or_default()
            .extend(errors);
    }
}

// The function a call reaches, by file and name, when it is one whose frame a parameter governs.
fn governed_target<'a>(
    fs: &dyn crate::ports::FileSystem,
    call: &ast::ExprCall,
    file_path: &str,
    project_root: &Path,
    files: &'a HashMap<String, IndexEntry>,
) -> Option<(String, String, &'a ParamGovernedTemplate)> {
    let callee_name = match &*call.func {
        Expr::Name(n) => n.id.as_str(),
        Expr::Attribute(attr) => attr.attr.as_str(),
        _ => return None,
    };
    let (target_file, target_func) =
        resolve_delegate_target(fs, file_path, callee_name, project_root, files)?;
    let template = files
        .get(&target_file)
        .and_then(|e| e.functions.get(&target_func))?
        .param_governed
        .as_ref()?;
    Some((target_file, target_func, template))
}

// The argument a call passes for the governing parameter, by keyword or by position.
fn governing_argument<'c>(
    call: &'c ast::ExprCall,
    template: &ParamGovernedTemplate,
) -> Option<&'c Expr> {
    call.arguments
        .keywords
        .iter()
        .find(|k| k.arg.as_ref().map(|s| s.as_str()) == Some(template.param_name.as_str()))
        .map(|k| &k.value)
        .or_else(|| call.arguments.args.get(template.param_index))
}

// What a governed call reports: that its columns can't be known, or each column the callee reads
// that the call's resolved features don't have.
fn governed_call_errors(
    (line, col): (usize, usize),
    template: &ParamGovernedTemplate,
    target_file: &str,
    target_func: &str,
    resolved_cols: Option<Vec<String>>,
) -> Vec<LintError> {
    let Some(resolved_cols) = resolved_cols else {
        return vec![LintError {
            line,
            col,
            code: CODE_UNTRACKED_DATAFRAME.to_string(),
            message: format!(
                "columns unknown at lint time; the `{}` argument passed here isn't \
                 a literal (or traceable to one), so column access inside '{}' can't \
                 be validated for this call",
                template.param_name, target_func
            ),
            severity: "warning".to_string(),
        }];
    };
    template
        .accesses
        .iter()
        .filter(|access| !resolved_cols.iter().any(|c| c == &access.column))
        .map(|access| LintError {
            line,
            col,
            code: CODE_UNKNOWN_COLUMN.to_string(),
            message: format!(
                "Column '{}' does not exist for this call's resolved features {:?} \
                     (accessed at {}:{}:{} inside '{}')",
                access.column, resolved_cols, target_file, access.line, access.col, target_func
            ),
            severity: "error".to_string(),
        })
        .collect()
}

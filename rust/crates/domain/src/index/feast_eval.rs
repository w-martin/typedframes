use super::*;

// General lazy evaluator for a feature-list-producing expression: a literal list, a
// literal string/f-string built from an environment of substituted parameter values,
// or a call to another function -- in which case the call's own argument expressions
// are evaluated in the CURRENT environment, bound to the callee's parameter names, and
// the callee's first return statement is evaluated recursively in that new
// environment. This subsumes direct literals, zero-arg forwarding, AND argument
// substitution in one general mechanism. `visiting` breaks cycles (mutually- or
// self-delegating chains) the same way resolve_node_requires does elsewhere.
pub(crate) fn eval_feast_list_expr(
    fs: &dyn crate::ports::FileSystem,
    expr: &Expr,
    env: &HashMap<String, String>,
    file: &str,
    project_root: &Path,
    files: &HashMap<String, IndexEntry>,
    visiting: &mut std::collections::HashSet<FuncNode>,
) -> Option<Vec<String>> {
    match expr {
        Expr::List(list) => list
            .elts
            .iter()
            .map(|e| eval_feast_string_expr(e, env))
            .collect(),
        Expr::Call(call) => eval_feast_call(fs, call, env, file, project_root, files, visiting),
        _ => None,
    }
}

pub(crate) fn eval_feast_string_expr(expr: &Expr, env: &HashMap<String, String>) -> Option<String> {
    match expr {
        Expr::StringLiteral(s) => Some(s.value.to_str().to_string()),
        Expr::Name(n) => env.get(n.id.as_str()).cloned(),
        Expr::FString(f) => eval_fstring_expr(f, env),
        _ => None,
    }
}

pub(crate) fn eval_fstring_expr(
    fstring: &ast::ExprFString,
    env: &HashMap<String, String>,
) -> Option<String> {
    let mut result = String::new();
    for part in fstring.value.as_slice() {
        eval_fstring_part(env, &mut result, part)?;
    }
    Some(result)
}

fn eval_fstring_part(
    env: &HashMap<String, String>,
    result: &mut String,
    part: &ruff_python_ast::FStringPart,
) -> Option<()> {
    let _: () = match part {
        ast::FStringPart::Literal(lit) => result.push_str(&lit.value),
        ast::FStringPart::FString(f) => {
            eval_fstring_elements(env, result, f)?;
        }
    };
    Some(())
}

fn eval_fstring_elements(
    env: &HashMap<String, String>,
    result: &mut String,
    f: &ruff_python_ast::FString,
) -> Option<()> {
    let _: () = for element in f.elements.iter() {
        match element {
            ast::InterpolatedStringElement::Literal(lit) => {
                result.push_str(&lit.value);
            }
            ast::InterpolatedStringElement::Interpolation(interp) => {
                if interp.conversion != ast::ConversionFlag::None || interp.format_spec.is_some() {
                    return None;
                }
                let Expr::Name(name) = &*interp.expression else {
                    return None;
                };
                result.push_str(env.get(name.id.as_str())?);
            }
        }
    };
    Some(())
}

pub(crate) fn eval_feast_call(
    fs: &dyn crate::ports::FileSystem,
    call: &ast::ExprCall,
    env: &HashMap<String, String>,
    file: &str,
    project_root: &Path,
    files: &HashMap<String, IndexEntry>,
    visiting: &mut std::collections::HashSet<FuncNode>,
) -> Option<Vec<String>> {
    if !call.arguments.keywords.is_empty() {
        return None;
    }
    let callee_name = match &*call.func {
        Expr::Name(n) => n.id.as_str(),
        Expr::Attribute(a) => a.attr.as_str(),
        _ => return None,
    };
    let (target_file, target_func) =
        resolve_delegate_target(fs, file, callee_name, project_root, files)?;
    let node = (target_file.clone(), target_func.clone());
    if visiting.contains(&node) {
        return None;
    }

    let mut arg_values = Vec::with_capacity(call.arguments.args.len());
    for arg in &call.arguments.args {
        arg_values.push(eval_feast_string_expr(arg, env)?);
    }

    let source = fs.read_to_string(Path::new(&target_file)).ok()?;
    let parsed = parse_module(&source).ok()?;
    let module = parsed.into_syntax();
    let func_def = find_function_def_by_name(&module.body, &target_func)?;

    let param_names: Vec<String> = func_def
        .parameters
        .posonlyargs
        .iter()
        .chain(func_def.parameters.args.iter())
        .map(|p| p.parameter.name.id.to_string())
        .collect();
    if param_names.len() != arg_values.len() {
        return None;
    }
    let new_env: HashMap<String, String> = param_names.into_iter().zip(arg_values).collect();

    let return_expr = find_first_return_expr(&func_def.body)?;

    visiting.insert(node.clone());
    let result = eval_feast_list_expr(
        fs,
        return_expr,
        &new_env,
        &target_file,
        project_root,
        files,
        visiting,
    );
    visiting.remove(&node);
    result
}

pub(crate) fn find_function_def_by_name<'a>(
    stmts: &'a [Stmt],
    name: &str,
) -> Option<&'a ast::StmtFunctionDef> {
    for stmt in stmts {
        match stmt {
            Stmt::FunctionDef(f) if f.name.as_str() == name => return Some(f),
            Stmt::If(if_stmt) => {
                if let Some(f) = find_function_def_by_name(&if_stmt.body, name) {
                    return Some(f);
                }
                for clause in &if_stmt.elif_else_clauses {
                    if let Some(f) = find_function_def_by_name(&clause.body, name) {
                        return Some(f);
                    }
                }
            }
            Stmt::For(for_stmt) => {
                if let Some(f) = find_function_def_by_name(&for_stmt.body, name) {
                    return Some(f);
                }
            }
            Stmt::While(while_stmt) => {
                if let Some(f) = find_function_def_by_name(&while_stmt.body, name) {
                    return Some(f);
                }
            }
            Stmt::With(with_stmt) => {
                if let Some(f) = find_function_def_by_name(&with_stmt.body, name) {
                    return Some(f);
                }
            }
            _ => {}
        }
    }
    None
}

pub(crate) fn find_first_return_expr(stmts: &[Stmt]) -> Option<&Expr> {
    for stmt in stmts {
        match stmt {
            Stmt::Return(ret) => {
                if let Some(v) = &ret.value {
                    return Some(v);
                }
            }
            Stmt::If(if_stmt) => {
                if let Some(e) = find_first_return_expr(&if_stmt.body) {
                    return Some(e);
                }
                for clause in &if_stmt.elif_else_clauses {
                    if let Some(e) = find_first_return_expr(&clause.body) {
                        return Some(e);
                    }
                }
            }
            Stmt::For(for_stmt) => {
                if let Some(e) = find_first_return_expr(&for_stmt.body) {
                    return Some(e);
                }
            }
            Stmt::While(while_stmt) => {
                if let Some(e) = find_first_return_expr(&while_stmt.body) {
                    return Some(e);
                }
            }
            Stmt::With(with_stmt) => {
                if let Some(e) = find_first_return_expr(&with_stmt.body) {
                    return Some(e);
                }
            }
            _ => {}
        }
    }
    None
}

use super::*;

// `def run(name, fn, *args): return fn(*args)`: (position of `fn`, positional parameters before
// `*args`). `fn` must be a positional parameter, called with exactly `*args` and nothing else,
// and that call's result must be all the function ever returns: a wrapper that post-processes
// the result, or forwards keywords, does not hand back what `fn` returns for those arguments.
pub(super) fn forwarder_shape(func_def: &ast::StmtFunctionDef) -> Option<(usize, usize)> {
    let is_forward = forwarded_call();
    let parameters = &func_def.parameters;
    let varargs = parameters.vararg.as_ref()?.name.as_str();
    if parameters.kwarg.is_some() {
        return None;
    }
    let mut returns = Returns {
        values: Vec::new(),
        bare: false,
    };
    func_def
        .body
        .iter()
        .for_each(|stmt| returns.visit_stmt(stmt));
    if returns.bare || returns.values.is_empty() {
        return None;
    }
    let positional: Vec<&str> = parameters
        .posonlyargs
        .iter()
        .chain(parameters.args.iter())
        .map(|p| p.parameter.name.as_str())
        .collect();
    positional.iter().enumerate().find_map(|(index, callable)| {
        // `result = fn(*args)` ... `return result`, with `result` bound nowhere else.
        let bound_result = func_def.body.iter().find_map(|stmt| match stmt {
            Stmt::Assign(assign) => match assign.targets.as_slice() {
                [Expr::Name(target)] if is_forward(&assign.value, callable, varargs) => {
                    Some(target.id.as_str())
                }
                _ => None,
            },
            _ => None,
        });
        let rebound = |name: &str| {
            func_def
                .body
                .iter()
                .filter(|stmt| {
                    matches!(stmt, Stmt::Assign(a) if a.targets.iter().any(
                        |t| matches!(t, Expr::Name(n) if n.id.as_str() == name)))
                })
                .count()
                > 1
        };
        returns
            .values
            .iter()
            .all(|value| {
                is_forward(value, callable, varargs)
                    || matches!(value, Expr::Name(n)
                        if Some(n.id.as_str()) == bound_result && !rebound(n.id.as_str()))
            })
            .then_some((index, positional.len()))
    })
}

pub(super) struct Returns<'a> {
    pub(super) values: Vec<&'a Expr>,
    pub(super) bare: bool,
}

impl<'a> Visitor<'a> for Returns<'a> {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::FunctionDef(_) | Stmt::ClassDef(_) => {}
            Stmt::Return(ret) => match ret.value.as_deref() {
                Some(value) => self.values.push(value),
                None => self.bare = true,
            },
            _ => ast_visitor::walk_stmt(self, stmt),
        }
    }
}

fn forwarded_call() -> impl Fn(&Expr, &str, &str) -> bool {
    let is_forward = |expr: &Expr, callable: &str, varargs: &str| match expr {
        Expr::Call(call) => {
            matches!(&*call.func, Expr::Name(n) if n.id.as_str() == callable)
                && call.arguments.keywords.is_empty()
                && matches!(
                    call.arguments.args.as_ref(),
                    [Expr::Starred(star)]
                        if matches!(&*star.value, Expr::Name(n) if n.id.as_str() == varargs)
                )
        }
        _ => false,
    };
    is_forward
}

fn is_type_checking_guard(test: &Expr) -> bool {
    match test {
        Expr::Name(name) => name.id.as_str() == "TYPE_CHECKING",
        Expr::Attribute(attr) => {
            attr.attr.as_str() == "TYPE_CHECKING"
                && matches!(&*attr.value, Expr::Name(base) if matches!(base.id.as_str(), "typing" | "t"))
        }
        _ => false,
    }
}

// The module's top-level statements plus the bodies of `if TYPE_CHECKING:` blocks. The
// block never runs, but the checker never runs the program either, and the names it
// imports are exactly the ones annotations refer to.
pub(crate) fn top_level_and_type_checking_stmts(module: &ModModule) -> Vec<&Stmt> {
    let mut out = Vec::new();
    for stmt in &module.body {
        match stmt {
            Stmt::If(if_stmt) if is_type_checking_guard(&if_stmt.test) => {
                out.extend(&if_stmt.body);
            }
            _ => out.push(stmt),
        }
    }
    out
}

// The expression a function returns, when it is the only value-return in its body
// (nested functions and classes aside): a second return may hand back something else, so no
// one return describes what callers get.
pub(super) fn only_returned_expr(stmts: &[Stmt]) -> Option<&Expr> {
    let mut returned = Vec::new();
    collect_returned_exprs(stmts, &mut returned);
    match returned.as_slice() {
        [Expr::Name(n)] if n.id.as_str() == crate::returned_loads::LIFTED_NAME => {
            lifted_load(stmts)
        }
        [expr] => Some(expr),
        _ => None,
    }
}

fn collect_returned_exprs<'a>(stmts: &'a [Stmt], out: &mut Vec<&'a Expr>) {
    for stmt in stmts {
        match stmt {
            Stmt::Return(ret) => out.extend(ret.value.as_deref()),
            Stmt::If(s) => {
                collect_returned_exprs(&s.body, out);
                for clause in &s.elif_else_clauses {
                    collect_returned_exprs(&clause.body, out);
                }
            }
            Stmt::For(s) => {
                collect_returned_exprs(&s.body, out);
                collect_returned_exprs(&s.orelse, out);
            }
            Stmt::While(s) => {
                collect_returned_exprs(&s.body, out);
                collect_returned_exprs(&s.orelse, out);
            }
            Stmt::With(s) => collect_returned_exprs(&s.body, out),
            Stmt::Try(s) => {
                collect_returned_exprs(&s.body, out);
                for handler in &s.handlers {
                    let ast::ExceptHandler::ExceptHandler(h) = handler;
                    collect_returned_exprs(&h.body, out);
                }
                collect_returned_exprs(&s.orelse, out);
                collect_returned_exprs(&s.finalbody, out);
            }
            Stmt::Match(s) => s
                .cases
                .iter()
                .for_each(|c| collect_returned_exprs(&c.body, out)),
            _ => {}
        }
    }
}

// The load `crate::returned_loads` bound to a name just before returning it.
fn lifted_load<'a>(stmts: &'a [Stmt]) -> Option<&'a Expr> {
    for stmt in stmts {
        let nested: Vec<&'a [Stmt]> = match stmt {
            Stmt::Assign(a)
                if matches!(a.targets.as_slice(), [Expr::Name(n)] if n.id.as_str() == crate::returned_loads::LIFTED_NAME)
                    && matches!(&*a.value, Expr::Call(_)) =>
            {
                return Some(&a.value);
            }
            Stmt::If(s) => std::iter::once(&s.body[..])
                .chain(s.elif_else_clauses.iter().map(|c| &c.body[..]))
                .collect(),
            Stmt::For(s) => vec![&s.body[..], &s.orelse[..]],
            Stmt::While(s) => vec![&s.body[..], &s.orelse[..]],
            Stmt::With(s) => vec![&s.body[..]],
            Stmt::Try(s) => vec![&s.body[..], &s.orelse[..], &s.finalbody[..]],
            _ => continue,
        };
        if let Some(found) = nested.into_iter().find_map(lifted_load) {
            return Some(found);
        }
    }
    None
}

pub(super) fn only_returned_call(stmts: &[Stmt]) -> Option<&ast::ExprCall> {
    match only_returned_expr(stmts)? {
        Expr::Call(call) => Some(call),
        _ => None,
    }
}

// Walk `stmts` looking for the first `return <Name>` — handles top-level returns
// and those nested inside `if`/`for`/`while`/`with` bodies.  Returns the variable
// name as an owned String, or None if no bare-name return is found.
pub(super) fn find_returned_var(stmts: &[Stmt]) -> Option<String> {
    for stmt in stmts {
        match stmt {
            Stmt::Return(ret) => {
                if let Some(value) = &ret.value {
                    if let Expr::Name(name) = value.as_ref() {
                        return Some(name.id.to_string());
                    }
                }
            }
            Stmt::If(if_stmt) => {
                if let Some(var) = find_returned_var(&if_stmt.body) {
                    return Some(var);
                }
                for clause in &if_stmt.elif_else_clauses {
                    if let Some(var) = find_returned_var(&clause.body) {
                        return Some(var);
                    }
                }
            }
            Stmt::For(for_stmt) => {
                if let Some(var) = find_returned_var(&for_stmt.body) {
                    return Some(var);
                }
            }
            Stmt::While(while_stmt) => {
                if let Some(var) = find_returned_var(&while_stmt.body) {
                    return Some(var);
                }
            }
            Stmt::With(with_stmt) => {
                if let Some(var) = find_returned_var(&with_stmt.body) {
                    return Some(var);
                }
            }
            _ => {}
        }
    }
    None
}

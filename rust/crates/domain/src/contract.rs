//! Tainted-parameter / column-contract inference over a function body.
//!
//! Determines which columns a function implicitly requires of a DataFrame passed
//! into it, by tracking which locals are forwarded from a tainted parameter.

use ruff_python_ast::visitor::{self as ast_visitor, Visitor};
use ruff_python_ast::{self as ast, Expr, Stmt};

/// Whether `expr` is a method chain that starts from a tainted name (`frame.group_by("a")`).
fn chain_starts_at_tainted(tainted: &std::collections::HashSet<String>, expr: &Expr) -> bool {
    match expr {
        Expr::Name(n) => tainted.contains(n.id.as_str()),
        Expr::Call(call) => chain_starts_at_tainted(tainted, &call.func),
        Expr::Attribute(attr) => chain_starts_at_tainted(tainted, &attr.value),
        _ => false,
    }
}

/// Column names a polars chain on a tainted frame reads: `pl.col("x")` in the arguments of the
/// expression methods, plus string arguments to `select`, `group_by` and `sort`.
fn polars_reads(call: &ast::ExprCall, method: &str) -> Vec<String> {
    if !matches!(
        method,
        "select" | "with_columns" | "filter" | "group_by" | "agg" | "sort"
    ) {
        return Vec::new();
    }
    let literal = matches!(method, "select" | "group_by" | "sort");
    let mut names = Vec::new();
    for arg in call
        .arguments
        .args
        .iter()
        .chain(call.arguments.keywords.iter().map(|k| &k.value))
    {
        names.extend(crate::ast_extract::collect_pl_col_names(arg));
        if literal {
            names
                .extend(crate::ast_extract::extract_string_list_or_single(arg).unwrap_or_default());
        }
    }
    names
}

/// Names the function makes itself (`.alias("x")`, `with_columns(x=..)`, `df["x"] = ..`), which
/// a caller's frame need not have.
pub(crate) fn created_names(body: &[Stmt]) -> std::collections::HashSet<String> {
    let mut created = Created::default();
    for stmt in body {
        created.visit_stmt(stmt);
    }
    created.0
}

#[derive(Default)]
struct Created(std::collections::HashSet<String>);

impl<'a> Visitor<'a> for Created {
    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::Call(call) => {
                self.expr_call(call);
            }
            Expr::Subscript(sub) if sub.ctx == ast::ExprContext::Store => {
                self.0.extend(
                    crate::ast_extract::extract_string_list_or_single(&sub.slice)
                        .unwrap_or_default(),
                );
            }
            _ => {}
        }
        ast_visitor::walk_expr(self, expr);
    }
}

impl Created {
    fn expr_call(&mut self, call: &ruff_python_ast::ExprCall) {
        if let Expr::Attribute(attr) = &*call.func {
            if attr.attr.as_str() == "alias" {
                if let Some(name) = call
                    .arguments
                    .args
                    .first()
                    .and_then(crate::ast_extract::extract_string_literal)
                {
                    self.0.insert(name.to_string());
                }
            }
        }
        for keyword in &call.arguments.keywords {
            if let Some(name) = &keyword.arg {
                self.0.insert(name.to_string());
            }
        }
    }
}

pub(crate) fn expr_forwards_tainted(
    tainted: &std::collections::HashSet<String>,
    expr: &Expr,
) -> bool {
    match expr {
        Expr::Name(n) => tainted.contains(n.id.as_str()),
        Expr::Call(call) => call
            .arguments
            .args
            .first()
            .is_some_and(|a| matches!(a, Expr::Name(n) if tainted.contains(n.id.as_str()))),
        _ => false,
    }
}

// Recursively scan `expr` for two things, given the current set of variable names
// considered aliases of the function's own first parameter:
//   - `direct`: columns subscripted directly off a tainted variable, both a single
//     string (`tainted["col"]`) and a list (`tainted[["a", "b"]]`)
//   - `delegates`: names of functions called with a tainted variable as their first
//     positional argument — candidates for transitive requirement resolution
//     (see resolve_transitive_requires), since the parameter itself carries no
//     schema here and we can't validate the access ourselves, only record it.
pub(crate) fn scan_expr_for_contract(
    tainted: &std::collections::HashSet<String>,
    expr: &Expr,
    direct: &mut Vec<String>,
    delegates: &mut Vec<String>,
) {
    match expr {
        Expr::Subscript(subscript) => {
            if let Expr::Name(base) = &*subscript.value {
                if tainted.contains(base.id.as_str()) {
                    if let Some(cols) =
                        crate::ast_extract::extract_string_list_or_single(&subscript.slice)
                    {
                        direct.extend(cols);
                    }
                }
            }
            scan_expr_for_contract(tainted, &subscript.value, direct, delegates);
            scan_expr_for_contract(tainted, &subscript.slice, direct, delegates);
        }
        Expr::Attribute(attr) => {
            scan_expr_for_contract(tainted, &attr.value, direct, delegates);
        }
        Expr::Call(call) => {
            if let Expr::Attribute(attr) = &*call.func {
                if chain_starts_at_tainted(tainted, &attr.value) {
                    direct.extend(polars_reads(call, attr.attr.as_str()));
                }
            }
            let first_arg_tainted = call
                .arguments
                .args
                .first()
                .is_some_and(|a| matches!(a, Expr::Name(n) if tainted.contains(n.id.as_str())));
            match &*call.func {
                Expr::Name(func_name) if first_arg_tainted => {
                    delegates.push(func_name.id.to_string());
                }
                // `module.func(df)` via a plain `import module` — record the bare
                // attribute name as a delegate candidate the same way a bare-name
                // call would be (resolve_delegate_target follows it back to the
                // module). Guarded on `base` not itself being tainted so an actual
                // DataFrame method call (`df.merge(df)`) is never misread as a
                // delegate to a function literally named `merge`.
                Expr::Attribute(attr) if first_arg_tainted => {
                    if let Expr::Name(base) = &*attr.value {
                        if !tainted.contains(base.id.as_str()) {
                            delegates.push(attr.attr.to_string());
                        }
                    }
                }
                _ => {}
            }
            scan_expr_for_contract(tainted, &call.func, direct, delegates);
            for arg in &call.arguments.args {
                scan_expr_for_contract(tainted, arg, direct, delegates);
            }
            for kw in &call.arguments.keywords {
                scan_expr_for_contract(tainted, &kw.value, direct, delegates);
            }
        }
        Expr::BinOp(binop) => {
            scan_expr_for_contract(tainted, &binop.left, direct, delegates);
            scan_expr_for_contract(tainted, &binop.right, direct, delegates);
        }
        Expr::BoolOp(boolop) => {
            for v in &boolop.values {
                scan_expr_for_contract(tainted, v, direct, delegates);
            }
        }
        Expr::UnaryOp(unary) => {
            scan_expr_for_contract(tainted, &unary.operand, direct, delegates);
        }
        Expr::Compare(compare) => {
            scan_expr_for_contract(tainted, &compare.left, direct, delegates);
            for comp in compare.comparators.iter() {
                scan_expr_for_contract(tainted, comp, direct, delegates);
            }
        }
        Expr::List(list) => {
            for el in &list.elts {
                scan_expr_for_contract(tainted, el, direct, delegates);
            }
        }
        Expr::Tuple(tuple) => {
            for el in &tuple.elts {
                scan_expr_for_contract(tainted, el, direct, delegates);
            }
        }
        _ => {}
    }
}

// Statement-level counterpart to scan_expr_for_contract — covers the handful of
// statement shapes that appear in typical single-purpose transform functions
// (return, bare expr, assign, if/for/while/with). Not an exhaustive AST walk;
// deeply nested control flow may under-report requirements/delegates. `tainted`
// grows as assignments are discovered, so later statements in the same body see
// earlier aliasing — this is a single top-to-bottom pass, not full control-flow
// analysis: taint picked up inside an `if`/`for` body is visible afterwards, and
// both branches of an `if` contribute to the same taint set (conservative).
pub(crate) fn analyze_stmt_for_contract(
    stmt: &Stmt,
    tainted: &mut std::collections::HashSet<String>,
    direct: &mut Vec<String>,
    delegates: &mut Vec<String>,
) {
    match stmt {
        Stmt::Return(ret) => {
            if let Some(value) = &ret.value {
                scan_expr_for_contract(tainted, value, direct, delegates);
            }
        }
        Stmt::Expr(expr_stmt) => {
            scan_expr_for_contract(tainted, &expr_stmt.value, direct, delegates);
        }
        Stmt::Assign(assign) => {
            scan_expr_for_contract(tainted, &assign.value, direct, delegates);
            if expr_forwards_tainted(tainted, &assign.value) {
                for target in &assign.targets {
                    if let Expr::Name(t) = target {
                        tainted.insert(t.id.to_string());
                    }
                }
            }
        }
        Stmt::AnnAssign(ann_assign) => {
            if let Some(value) = &ann_assign.value {
                scan_expr_for_contract(tainted, value, direct, delegates);
                if expr_forwards_tainted(tainted, value) {
                    if let Expr::Name(t) = &*ann_assign.target {
                        tainted.insert(t.id.to_string());
                    }
                }
            }
        }
        Stmt::If(if_stmt) => {
            scan_expr_for_contract(tainted, &if_stmt.test, direct, delegates);
            for s in &if_stmt.body {
                analyze_stmt_for_contract(s, tainted, direct, delegates);
            }
            for clause in &if_stmt.elif_else_clauses {
                for s in &clause.body {
                    analyze_stmt_for_contract(s, tainted, direct, delegates);
                }
            }
        }
        Stmt::For(for_stmt) => {
            scan_expr_for_contract(tainted, &for_stmt.iter, direct, delegates);
            for s in &for_stmt.body {
                analyze_stmt_for_contract(s, tainted, direct, delegates);
            }
        }
        Stmt::While(while_stmt) => {
            scan_expr_for_contract(tainted, &while_stmt.test, direct, delegates);
            for s in &while_stmt.body {
                analyze_stmt_for_contract(s, tainted, direct, delegates);
            }
        }
        Stmt::With(with_stmt) => {
            for item in &with_stmt.items {
                scan_expr_for_contract(tainted, &item.context_expr, direct, delegates);
            }
            for s in &with_stmt.body {
                analyze_stmt_for_contract(s, tainted, direct, delegates);
            }
        }
        _ => {}
    }
}

// Validate a call to a function with a known parameter contract (`self.param_requires`)
// against the tracked schema of its first positional argument.  Fires at the call
// site — pipeline.py, not inside the function itself — because that's where the
// argument's actual inferred schema is known.

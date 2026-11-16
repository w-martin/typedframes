//! Loads in return position (`return pd.read_csv(..)`) are origins like `df = pd.read_csv(..)`
//! followed by `return df`. They are rewritten that way before the walk, with the source
//! ranges of the original statement, so counting, warnings and the fixer treat both alike.

use ruff_python_ast::name::Name;
use ruff_python_ast::{self as ast, Expr, ExprContext, ModModule, Stmt};

use crate::constants::{LOAD_FUNCTIONS, LOAD_MODULES};

pub(crate) const LIFTED_NAME: &str = "result";

fn is_module_load(expr: &Expr) -> bool {
    let Expr::Call(call) = expr else {
        return false;
    };
    let Expr::Attribute(attr) = &*call.func else {
        return false;
    };
    matches!(&*attr.value, Expr::Name(base) if LOAD_MODULES.contains(&base.id.as_str()))
        && LOAD_FUNCTIONS.contains(&attr.attr.as_str())
        // A constructor from in-memory rows is not a file read a column selection could narrow.
        && attr.attr.as_str() != "DataFrame"
}

fn name_expr(ctx: ExprContext, range: ruff_text_size::TextRange) -> Expr {
    Expr::Name(ast::ExprName {
        node_index: ast::AtomicNodeIndex::default(),
        range,
        id: Name::new(LIFTED_NAME),
        ctx,
    })
}

/// How many `return <value>` statements `body` has, not counting nested functions and classes.
fn value_returns(body: &[Stmt]) -> usize {
    body.iter()
        .map(|stmt| match stmt {
            Stmt::Return(ret) => usize::from(ret.value.is_some()),
            Stmt::If(s) => {
                value_returns(&s.body)
                    + s.elif_else_clauses
                        .iter()
                        .map(|c| value_returns(&c.body))
                        .sum::<usize>()
            }
            Stmt::For(s) => value_returns(&s.body) + value_returns(&s.orelse),
            Stmt::While(s) => value_returns(&s.body) + value_returns(&s.orelse),
            Stmt::With(s) => value_returns(&s.body),
            Stmt::Try(s) => {
                value_returns(&s.body)
                    + s.handlers
                        .iter()
                        .map(|h| {
                            let ast::ExceptHandler::ExceptHandler(h) = h;
                            value_returns(&h.body)
                        })
                        .sum::<usize>()
                    + value_returns(&s.orelse)
                    + value_returns(&s.finalbody)
            }
            Stmt::Match(s) => s.cases.iter().map(|c| value_returns(&c.body)).sum(),
            _ => 0,
        })
        .sum()
}

fn lift_body<V>(body: &mut V, in_function: bool, changed: &mut bool)
where
    V: Default + IntoIterator<Item = Stmt> + FromIterator<Stmt>,
{
    let mut lifted = Vec::new();
    for mut stmt in std::mem::take(body) {
        match &mut stmt {
            Stmt::FunctionDef(f) => {
                // A declared schema already governs what the function returns.
                let declared = f.returns.as_deref().is_some_and(|r| {
                    crate::ast_extract::extract_schema_from_annotation(r).is_some()
                });
                // Several returns give callers no single frame to follow.
                let liftable = !declared && value_returns(&f.body) == 1;
                lift_body(&mut f.body, liftable, changed)
            }
            Stmt::ClassDef(c) => lift_body(&mut c.body, false, changed),
            Stmt::If(s) => {
                lift_body(&mut s.body, in_function, changed);
                for clause in &mut s.elif_else_clauses {
                    lift_body(&mut clause.body, in_function, changed);
                }
            }
            Stmt::For(s) => {
                lift_body(&mut s.body, in_function, changed);
                lift_body(&mut s.orelse, in_function, changed);
            }
            Stmt::While(s) => {
                lift_body(&mut s.body, in_function, changed);
                lift_body(&mut s.orelse, in_function, changed);
            }
            Stmt::With(s) => lift_body(&mut s.body, in_function, changed),
            Stmt::Try(s) => {
                lift_body(&mut s.body, in_function, changed);
                for handler in &mut s.handlers {
                    let ast::ExceptHandler::ExceptHandler(h) = handler;
                    lift_body(&mut h.body, in_function, changed);
                }
                lift_body(&mut s.orelse, in_function, changed);
                lift_body(&mut s.finalbody, in_function, changed);
            }
            _ => {}
        }
        let Stmt::Return(ret) = &mut stmt else {
            lifted.push(stmt);
            continue;
        };
        if !ret
            .value
            .as_deref()
            .is_some_and(|v| in_function && is_module_load(v))
        {
            lifted.push(stmt);
            continue;
        }
        let Some(value) = ret.value.take() else {
            continue;
        };
        *changed = true;
        let range = ret.range;
        lifted.push(Stmt::Assign(ast::StmtAssign {
            node_index: ast::AtomicNodeIndex::default(),
            range,
            targets: vec![name_expr(ExprContext::Store, range)],
            value,
        }));
        ret.value = Some(Box::new(name_expr(ExprContext::Load, range)));
        lifted.push(stmt);
    }
    *body = lifted.into_iter().collect();
}

/// `module` with its returned loads bound to a name first, or `None` when it has none.
pub(crate) fn lift(module: &ModModule) -> Option<ModModule> {
    let mut lifted = module.clone();
    let mut changed = false;
    lift_body(&mut lifted.body, false, &mut changed);
    changed.then_some(lifted)
}

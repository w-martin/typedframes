//! Frames held in a dict literal (`frames = {"train": pd.read_csv(..), ...}`) and read back
//! only through literal keys (`frames["train"]`) are checked like named frames: before the
//! walk, the dict assignment becomes one assignment per key to a name spelled `frames["train"]`,
//! and each `frames["train"]` read becomes that name, source ranges kept.
//!
//! Only when every mention of the dict in its scope (nested functions included) is such a
//! read: any other use (`.items()`, a computed key, a store, a rebinding) could change or
//! reach the frames behind the checker's back, so the dict is left alone.

use std::collections::{HashMap, HashSet};

use ruff_python_ast::name::Name;
use ruff_python_ast::visitor::transformer::{self as ast_transformer, Transformer};
use ruff_python_ast::visitor::{self as ast_visitor, Visitor};
use ruff_python_ast::{self as ast, Expr, ExprContext, ModModule, Stmt};
use ruff_text_size::{Ranged, TextRange};

fn key_name(dict: &str, key: &str) -> String {
    format!("{dict}[\"{key}\"]")
}

/// The string keys of a dict literal whose values are all calls or subscripts.
fn frame_dict_keys(dict: &ast::ExprDict) -> Option<Vec<String>> {
    let mut keys = Vec::new();
    for item in &dict.items {
        let key = crate::ast_extract::extract_string_literal(item.key.as_ref()?)?;
        if !matches!(item.value, Expr::Call(_) | Expr::Subscript(_))
            || keys.iter().any(|k| k == key)
        {
            return None;
        }
        keys.push(key.to_string());
    }
    (!keys.is_empty()).then_some(keys)
}

// For each candidate dict name: its keys, every mention, and the mentions that are reads
// through one of its keys. One walk counts all candidates of a scope.
struct Mentions<'a> {
    names: HashMap<&'a str, (&'a [String], usize, usize)>,
}

impl Mentions<'_> {
    fn mention(&mut self, name: &str) {
        if let Some(entry) = self.names.get_mut(name) {
            entry.1 += 1;
        }
    }

    fn expr_subscript(&mut self, sub: &ruff_python_ast::ExprSubscript) {
        if let (Expr::Name(n), ExprContext::Load) = (&*sub.value, sub.ctx) {
            if let Some(entry) = self.names.get_mut(n.id.as_str()) {
                if crate::ast_extract::extract_string_literal(&sub.slice)
                    .is_some_and(|k| entry.0.iter().any(|key| key == k))
                {
                    entry.2 += 1;
                }
            }
        }
    }
}

impl<'a> Visitor<'a> for Mentions<'_> {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            // `global`/`nonlocal`/parameters could rebind the name without an Expr::Name store.
            Stmt::Global(s) => s.names.iter().for_each(|n| self.mention(n.as_str())),
            Stmt::Nonlocal(s) => s.names.iter().for_each(|n| self.mention(n.as_str())),
            Stmt::FunctionDef(f) => f
                .parameters
                .iter()
                .for_each(|p| self.mention(p.name().as_str())),
            _ => {}
        }
        ast_visitor::walk_stmt(self, stmt);
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        if let Expr::Subscript(sub) = expr {
            self.expr_subscript(sub);
        }
        if let Expr::Lambda(lambda) = expr {
            if let Some(parameters) = &lambda.parameters {
                parameters
                    .iter()
                    .for_each(|p| self.mention(p.name().as_str()));
            }
        }
        if let Expr::Name(n) = expr {
            self.mention(n.id.as_str());
        }
        ast_visitor::walk_expr(self, expr);
    }
}

struct Rewriter<'a> {
    names: &'a HashSet<String>,
}

impl Rewriter<'_> {
    // `d["k"]` on a dict this pass expands: the variable that now holds that key's value.
    fn flattened(&self, expr: &Expr) -> Option<Expr> {
        let Expr::Subscript(sub) = expr else {
            return None;
        };
        let Expr::Name(n) = &*sub.value else {
            return None;
        };
        if !self.names.contains(n.id.as_str()) {
            return None;
        }
        let key = crate::ast_extract::extract_string_literal(&sub.slice)?;
        Some(Expr::Name(ast::ExprName {
            node_index: ast::AtomicNodeIndex::default(),
            range: sub.range,
            id: Name::new(key_name(n.id.as_str(), key)),
            ctx: ExprContext::Load,
        }))
    }
}

impl Transformer for Rewriter<'_> {
    fn visit_expr(&self, expr: &mut Expr) {
        if let Some(replacement) = self.flattened(expr) {
            *expr = replacement;
            return;
        }
        ast_transformer::walk_expr(self, expr);
    }
}

fn expand(name: &str, dict: &ast::ExprDict) -> Vec<Stmt> {
    dict.items
        .iter()
        .filter_map(|item| {
            let key = item.key.as_ref()?;
            let key_text = crate::ast_extract::extract_string_literal(key)?;
            Some(Stmt::Assign(ast::StmtAssign {
                node_index: ast::AtomicNodeIndex::default(),
                range: TextRange::new(key.start(), item.value.end()),
                targets: vec![Expr::Name(ast::ExprName {
                    node_index: ast::AtomicNodeIndex::default(),
                    range: key.range(),
                    id: Name::new(key_name(name, key_text)),
                    ctx: ExprContext::Store,
                })],
                value: Box::new(item.value.clone()),
            }))
        })
        .collect()
}

fn rewrite_scope<V>(stmts: &mut V, changed: &mut bool)
where
    V: Default + IntoIterator<Item = Stmt> + FromIterator<Stmt>,
{
    let mut body: Vec<Stmt> = std::mem::take(stmts).into_iter().collect();
    rewrite_vec(&mut body, changed);
    *stmts = body.into_iter().collect();
}

fn rewrite_vec(body: &mut Vec<Stmt>, changed: &mut bool) {
    let mut assigned: HashMap<&str, usize> = HashMap::new();
    let mut found: Vec<(usize, &str, Vec<String>)> = Vec::new();
    for (i, stmt) in body.iter().enumerate() {
        let Stmt::Assign(assign) = stmt else {
            continue;
        };
        let ([Expr::Name(target)], Expr::Dict(dict)) = (assign.targets.as_slice(), &*assign.value)
        else {
            continue;
        };
        if let Some(keys) = frame_dict_keys(dict) {
            *assigned.entry(target.id.as_str()).or_insert(0) += 1;
            found.push((i, target.id.as_str(), keys));
        }
    }
    // A name given two frame dicts is ambiguous: leave both.
    found.retain(|(_, name, _)| assigned[name] == 1);
    if found.is_empty() {
        return recurse(body, changed);
    }
    let mut mentions = Mentions {
        names: found
            .iter()
            .map(|(_, name, keys)| (*name, (keys.as_slice(), 0, 0)))
            .collect(),
    };
    for s in body.iter() {
        mentions.visit_stmt(s);
    }
    // The target itself is the one mention that is not a read.
    let candidates: HashMap<usize, String> = found
        .iter()
        .filter(|(_, name, _)| {
            let (_, total, allowed) = mentions.names[name];
            total == allowed + 1
        })
        .map(|(i, name, _)| (*i, name.to_string()))
        .collect();
    if candidates.is_empty() {
        return recurse(body, changed);
    }
    let old = std::mem::take(body);
    for (i, stmt) in old.into_iter().enumerate() {
        if let (Some(name), Stmt::Assign(assign)) = (candidates.get(&i), &stmt) {
            if let Expr::Dict(dict) = &*assign.value {
                body.extend(expand(name, dict));
                continue;
            }
        }
        body.push(stmt);
    }
    let names: HashSet<String> = candidates.into_values().collect();
    let rewriter = Rewriter { names: &names };
    for stmt in body.iter_mut() {
        rewriter.visit_stmt(stmt);
    }
    *changed = true;
    recurse(body, changed);
}

fn recurse(body: &mut [Stmt], changed: &mut bool) {
    for stmt in body.iter_mut() {
        if let Stmt::FunctionDef(f) = stmt {
            rewrite_scope(&mut f.body, changed);
        }
        if let Stmt::ClassDef(c) = stmt {
            for inner in c.body.iter_mut() {
                if let Stmt::FunctionDef(f) = inner {
                    rewrite_scope(&mut f.body, changed);
                }
            }
        }
    }
}

/// `module` with its frame dicts rewritten to names, or `None` when it has none.
pub(crate) fn rewrite(module: &ModModule) -> Option<ModModule> {
    let mut rewritten = module.clone();
    let mut changed = false;
    rewrite_scope(&mut rewritten.body, &mut changed);
    changed.then_some(rewritten)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruff_python_parser::parse_module;

    fn rewrites(source: &str) -> bool {
        rewrite(&parse_module(source).unwrap().into_syntax()).is_some()
    }

    #[test]
    fn test_should_rewrite_a_dict_read_only_through_literal_keys() {
        assert!(rewrites(
            "d = {\"a\": pd.read_csv(\"x\")}\nprint(d[\"a\"][\"c\"])\n"
        ));
    }

    #[test]
    fn test_should_rewrite_many_dicts_of_one_scope_in_linear_time() {
        // arrange
        let mut source = String::new();
        for i in 0..20_000 {
            source.push_str(&format!("d{i} = {{\"a\": f({i})}}\n"));
        }
        source.push_str("print(d7[\"a\"], d9)\n");
        let started = std::time::Instant::now();

        // act
        let rewritten = rewrite(&parse_module(&source).unwrap().into_syntax());

        // assert
        let body = rewritten.expect("rewritten").body;
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert!(matches!(&body[7], Stmt::Assign(a)
            if matches!(&a.targets[0], Expr::Name(n) if n.id.as_str() == "d7[\"a\"]")));
        assert!(matches!(&body[9], Stmt::Assign(a) if matches!(&*a.value, Expr::Dict(_))));
    }

    #[test]
    fn test_should_leave_a_dict_with_any_other_use() {
        assert!(!rewrites(
            "d = {\"a\": pd.read_csv(\"x\")}\nfor k, v in d.items():\n    pass\n"
        ));
        assert!(!rewrites(
            "d = {\"a\": pd.read_csv(\"x\")}\nd[\"a\"] = other\n"
        ));
        assert!(!rewrites(
            "d = {\"a\": pd.read_csv(\"x\")}\nprint(d[\"b\"])\n"
        ));
        assert!(!rewrites(
            "d = {\"a\": pd.read_csv(\"x\")}\ndef f(d):\n    return d[\"a\"]\n"
        ));
    }
}

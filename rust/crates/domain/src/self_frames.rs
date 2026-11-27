//! Frames stored on `self` (`self.df = pd.read_csv(..)`, `self.df = self.df.drop(..)`) are
//! checked like locals inside each method: every `self.<attr>` of such an attribute is
//! rewritten to a name spelled `self.<attr>` before the walk, source ranges kept.
//!
//! At a method's entry the attribute is an open frame (another method or the caller may have
//! changed it), so only facts established inside the same method are used. A call that can
//! reach `self` ends those facts again (see `Linter::forget_self_frames`).

use std::collections::{HashMap, HashSet};

use ruff_python_ast::name::Name;
use ruff_python_ast::visitor::transformer::{self as ast_transformer, Transformer};
use ruff_python_ast::visitor::{self as ast_visitor, Visitor};
use ruff_python_ast::{self as ast, Expr, ModModule, Stmt};
use ruff_text_size::{Ranged, TextSize};

use crate::constants::{LOAD_FUNCTIONS, LOAD_MODULES};

pub(crate) const SELF_PREFIX: &str = "self.";

/// The frame attributes each rewritten method starts with, by the method's start offset.
pub(crate) type MethodFrames = HashMap<TextSize, Vec<String>>;

fn self_attr(expr: &Expr) -> Option<&str> {
    let Expr::Attribute(attr) = expr else {
        return None;
    };
    matches!(&*attr.value, Expr::Name(n) if n.id.as_str() == "self").then(|| attr.attr.as_str())
}

fn frame_params(func: &ast::StmtFunctionDef) -> HashSet<String> {
    let params = &func.parameters;
    params
        .posonlyargs
        .iter()
        .chain(&params.args)
        .chain(&params.kwonlyargs)
        .filter(|p| {
            p.parameter
                .annotation
                .as_deref()
                .is_some_and(crate::ast_extract::extract_frame_parameter_type)
        })
        .map(|p| p.parameter.name.to_string())
        .collect()
}

// Whether `value` certainly evaluates to a DataFrame: a pandas/polars load, constructor,
// `concat`/`merge`, a frame parameter, or a method/subscript chain on a known frame attribute.
fn is_frame_value(value: &Expr, attrs: &HashSet<String>, params: &HashSet<String>) -> bool {
    match value {
        Expr::Name(n) => params.contains(n.id.as_str()),
        Expr::Call(call) => match &*call.func {
            Expr::Attribute(attr) => {
                if let Expr::Name(module) = &*attr.value {
                    if LOAD_MODULES.contains(&module.id.as_str()) {
                        let f = attr.attr.as_str();
                        return LOAD_FUNCTIONS.contains(&f) || matches!(f, "concat" | "merge");
                    }
                }
                is_frame_chain(&attr.value, attrs, params)
            }
            _ => false,
        },
        Expr::Subscript(sub) => is_frame_chain(&sub.value, attrs, params),
        _ => false,
    }
}

fn is_frame_chain(expr: &Expr, attrs: &HashSet<String>, params: &HashSet<String>) -> bool {
    if let Some(attr) = self_attr(expr) {
        return attrs.contains(attr);
    }
    match expr {
        Expr::Name(n) => params.contains(n.id.as_str()),
        Expr::Call(call) => match &*call.func {
            Expr::Attribute(attr) => is_frame_chain(&attr.value, attrs, params),
            _ => false,
        },
        Expr::Subscript(sub) => is_frame_chain(&sub.value, attrs, params),
        _ => false,
    }
}

struct Assignments<'a> {
    found: Vec<(&'a str, &'a Expr, Option<&'a Expr>)>,
}

impl<'a> Visitor<'a> for Assignments<'a> {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::FunctionDef(_) | Stmt::ClassDef(_) => return,
            Stmt::Assign(assign) => {
                for target in &assign.targets {
                    if let Some(attr) = self_attr(target) {
                        self.found.push((attr, &assign.value, None));
                    }
                }
            }
            Stmt::AnnAssign(ann) => {
                if let (Some(attr), Some(value)) = (self_attr(&ann.target), ann.value.as_deref()) {
                    self.found.push((attr, value, Some(&ann.annotation)));
                }
            }
            _ => {}
        }
        ast_visitor::walk_stmt(self, stmt);
    }
}

fn methods(class: &ast::StmtClassDef) -> impl Iterator<Item = &ast::StmtFunctionDef> {
    class.body.iter().filter_map(|stmt| match stmt {
        Stmt::FunctionDef(f)
            if f.parameters
                .args
                .first()
                .is_some_and(|p| p.parameter.name.as_str() == "self") =>
        {
            Some(f)
        }
        _ => None,
    })
}

/// The attributes of `class` that some method assigns a frame to.
fn frame_attributes(class: &ast::StmtClassDef) -> HashSet<String> {
    let mut per_method = Vec::new();
    for method in methods(class) {
        let mut visitor = Assignments { found: Vec::new() };
        for stmt in &method.body {
            visitor.visit_stmt(stmt);
        }
        per_method.push((frame_params(method), visitor.found));
    }
    let mut attrs = HashSet::new();
    loop {
        let before = attrs.len();
        for (params, found) in &per_method {
            for (attr, value, annotation) in found {
                let annotated =
                    annotation.is_some_and(crate::ast_extract::extract_frame_parameter_type);
                if annotated || is_frame_value(value, &attrs, params) {
                    attrs.insert((*attr).to_string());
                }
            }
        }
        if attrs.len() == before {
            return attrs;
        }
    }
}

struct Rewriter<'a> {
    attrs: &'a HashSet<String>,
}

impl Transformer for Rewriter<'_> {
    fn visit_expr(&self, expr: &mut Expr) {
        if let Expr::Call(call) = expr {
            // `self.method(..)` stays a method call.
            if self_attr(&call.func).is_some() {
                for arg in &mut call.arguments.args {
                    self.visit_expr(arg);
                }
                for keyword in &mut call.arguments.keywords {
                    self.visit_expr(&mut keyword.value);
                }
                return;
            }
        }
        if let Expr::Attribute(attr) = expr {
            if matches!(&*attr.value, Expr::Name(n) if n.id.as_str() == "self")
                && self.attrs.contains(attr.attr.as_str())
            {
                *expr = Expr::Name(ast::ExprName {
                    node_index: ast::AtomicNodeIndex::default(),
                    range: attr.range,
                    id: Name::new(format!("{SELF_PREFIX}{}", attr.attr)),
                    ctx: attr.ctx,
                });
                return;
            }
        }
        ast_transformer::walk_expr(self, expr);
    }
}

fn rewrite_body(body: &mut [Stmt], frames: &mut MethodFrames) {
    for stmt in body {
        rewrite_statement(frames, stmt);
    }
}

fn rewrite_statement(frames: &mut HashMap<TextSize, Vec<String>>, stmt: &mut Stmt) {
    match stmt {
        Stmt::ClassDef(class) => {
            rewrite_class(frames, class);
        }
        Stmt::FunctionDef(f) => rewrite_body(&mut f.body, frames),
        Stmt::If(s) => {
            rewrite_body(&mut s.body, frames);
            for clause in &mut s.elif_else_clauses {
                rewrite_body(&mut clause.body, frames);
            }
        }
        _ => {}
    }
}

fn rewrite_class(
    frames: &mut HashMap<TextSize, Vec<String>>,
    class: &mut ruff_python_ast::StmtClassDef,
) {
    let attrs = frame_attributes(class);
    if !attrs.is_empty() {
        let mut names: Vec<String> = attrs.iter().map(|a| format!("{SELF_PREFIX}{a}")).collect();
        names.sort();
        let rewriter = Rewriter { attrs: &attrs };
        for inner in &mut class.body {
            let Stmt::FunctionDef(f) = inner else {
                continue;
            };
            if f.parameters
                .args
                .first()
                .is_none_or(|p| p.parameter.name.as_str() != "self")
            {
                continue;
            }
            frames.insert(f.range().start(), names.clone());
            for s in &mut f.body {
                rewriter.visit_stmt(s);
            }
        }
    }
    rewrite_body(&mut class.body, frames);
}

/// `module` with frame attributes rewritten to names, and each method's frame attributes;
/// `None` when no class stores a frame on `self`.
pub(crate) fn rewrite(module: &ModModule) -> Option<(ModModule, MethodFrames)> {
    let mut rewritten = module.clone();
    let mut frames = MethodFrames::new();
    rewrite_body(&mut rewritten.body, &mut frames);
    (!frames.is_empty()).then_some((rewritten, frames))
}

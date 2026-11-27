//! Per-parameter column requirements of a function, for checking the frames a call passes.
//!
//! A parameter requires a column when the function reads it (`param["col"]`) on every
//! path from entry, before anything could have created it: only the top-level statements
//! up to the first branch, loop, `return` or other compound statement count, and a
//! parameter stops collecting at the first statement that writes to it, rebinds it, hands
//! it to another function or calls an in-place method on it. The first parameter also
//! keeps the older body-wide contract (`contract::analyze_stmt_for_contract`), but only for
//! a straight-line body (`is_straight_line`): a guarded read is no requirement.
//!
//! Nothing here is inferred from call sites: a requirement is a fact about the body alone.

use crate::contract;
use crate::frame_ops::FnKey;
use ruff_python_ast::visitor::{self as ast_visitor, Visitor};
use ruff_python_ast::{self as ast, Expr, Stmt};
use ruff_text_size::Ranged;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

mod certain_scan;

pub(crate) use certain_scan::*;

/// Slot prefix for a function brought in with `from module import name`.
pub(crate) const IMPORTED_SLOT: &str = "<import>";

/// The parameters a call can bind, after a bound `self`/`cls` is dropped.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Signature {
    pub(crate) positional: Vec<String>,
    pub(crate) posonly: usize,
    pub(crate) kwonly: Vec<String>,
    pub(crate) varargs: bool,
    pub(crate) kwargs: bool,
}

/// The arguments of a call: how many positional ones, then the keyword names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CallShape {
    pub(crate) positional: usize,
    pub(crate) keywords: Vec<String>,
}

impl CallShape {
    /// `None` for a call with `*args` or `**kwargs`, whose binding can't be known.
    pub(crate) fn of(call: &ast::ExprCall) -> Option<Self> {
        if call
            .arguments
            .args
            .iter()
            .any(|a| matches!(a, Expr::Starred(_)))
        {
            return None;
        }
        let keywords = call
            .arguments
            .keywords
            .iter()
            .map(|k| k.arg.as_ref().map(|a| a.to_string()))
            .collect::<Option<Vec<_>>>()?;
        Some(CallShape {
            positional: call.arguments.args.len(),
            keywords,
        })
    }
}

impl Signature {
    pub(crate) fn of(def: &ast::StmtFunctionDef, bound: bool) -> Self {
        let params = &def.parameters;
        let mut positional: Vec<String> = params
            .posonlyargs
            .iter()
            .chain(params.args.iter())
            .map(|p| p.parameter.name.to_string())
            .collect();
        let mut posonly = params.posonlyargs.len();
        if bound && !positional.is_empty() {
            positional.remove(0);
            posonly = posonly.saturating_sub(1);
        }
        Signature {
            positional,
            posonly,
            kwonly: params
                .kwonlyargs
                .iter()
                .map(|p| p.parameter.name.to_string())
                .collect(),
            varargs: params.vararg.is_some(),
            kwargs: params.kwarg.is_some(),
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.positional.len() + self.kwonly.len()
    }

    pub(crate) fn name(&self, index: usize) -> &str {
        match self.positional.get(index) {
            Some(name) => name,
            None => &self.kwonly[index - self.positional.len()],
        }
    }

    fn keyword_index(&self, name: &str) -> Option<usize> {
        if let Some(i) = self.positional.iter().position(|p| p == name) {
            return (i >= self.posonly).then_some(i);
        }
        self.kwonly
            .iter()
            .position(|p| p == name)
            .map(|i| i + self.positional.len())
    }

    /// The parameter each argument of `shape` binds to, in argument order (positional, then
    /// keywords); `None` inside for an argument absorbed by `*args`/`**kwargs`. `None` when
    /// the call can't bind (too many arguments, an unknown or repeated keyword).
    pub(crate) fn bind(&self, shape: &CallShape) -> Option<Vec<Option<usize>>> {
        let mut taken = vec![false; self.len()];
        let mut out = Vec::with_capacity(shape.positional + shape.keywords.len());
        if shape.positional > self.positional.len() && !self.varargs {
            return None;
        }
        for i in 0..shape.positional {
            let bound = (i < self.positional.len()).then_some(i);
            if let Some(i) = bound {
                taken[i] = true;
            }
            out.push(bound);
        }
        for keyword in &shape.keywords {
            match self.keyword_index(keyword) {
                Some(i) if taken[i] => return None,
                Some(i) => {
                    taken[i] = true;
                    out.push(Some(i));
                }
                None if self.kwargs => out.push(None),
                None => return None,
            }
        }
        Some(out)
    }
}

/// Where a forwarded parameter goes: before resolution the call's callee position (or a
/// `self.<method>` name); after, a key of the contract map or something only the project
/// index can resolve.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ForwardTarget {
    Call(u32),
    SelfMethod(String),
    Imported(String),
    Module(String, String),
}

/// A parameter passed straight to another function, as argument `argument` of a call
/// shaped `shape`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Forward {
    pub(crate) param: usize,
    pub(crate) target: ForwardTarget,
    pub(crate) shape: CallShape,
    pub(crate) argument: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct FuncContract {
    pub(crate) signature: Signature,
    /// Required columns per parameter, in `Signature` order (positional, then keyword-only).
    pub(crate) requires: Vec<Vec<String>>,
    pub(crate) def_line: usize,
    /// The parameter the body-wide first-parameter contract describes.
    pub(crate) loose_param: Option<usize>,
    #[serde(skip)]
    pub(crate) forwards: Vec<Forward>,
}

impl FuncContract {
    pub(crate) fn has_requirements(&self) -> bool {
        self.requires.iter().any(|r| !r.is_empty())
    }

    /// The same requirement, ignoring where the function is defined.
    pub(crate) fn agrees_with(&self, other: &FuncContract) -> bool {
        self.signature == other.signature && self.requires == other.requires
    }

    pub(crate) fn add_required(&mut self, param: usize, columns: &[String]) {
        let slot = &mut self.requires[param];
        for column in columns {
            if !slot.contains(column) {
                slot.push(column.clone());
            }
        }
        slot.sort();
    }

    /// Folds the requirement `target` places on the argument `forward` hands it.
    pub(crate) fn inherit(&mut self, forward: &Forward, target: &FuncContract) {
        let Some(bound) = target.signature.bind(&forward.shape) else {
            return;
        };
        if let Some(Some(index)) = bound.get(forward.argument) {
            let columns = target.requires[*index].clone();
            self.add_required(forward.param, &columns);
        }
    }
}

/// How a definition binds its first parameter, from its decorators; `None` for a decorator
/// that may change what a call does, or for an `async` function or generator, whose body
/// does not run at the call.
pub(crate) fn binding_of(def: &ast::StmtFunctionDef, in_class: bool) -> Option<bool> {
    if def.is_async || contains_yield(&def.body) {
        return None;
    }
    let mut bound = in_class;
    for decorator in &def.decorator_list {
        match crate::ast_extract::decorator_name(&decorator.expression) {
            Some("staticmethod") if in_class => bound = false,
            Some("classmethod") if in_class => bound = true,
            Some("override") => {}
            _ => return None,
        }
    }
    Some(bound)
}

fn contains_yield(body: &[Stmt]) -> bool {
    let mut finder = Finder(false);
    for stmt in body {
        finder.visit_stmt(stmt);
    }
    finder.0
}

struct Finder(bool);

impl<'a> Visitor<'a> for Finder {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        if !matches!(stmt, Stmt::FunctionDef(_) | Stmt::ClassDef(_)) {
            ast_visitor::walk_stmt(self, stmt);
        }
    }
    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::Yield(_) | Expr::YieldFrom(_) => self.0 = true,
            Expr::Lambda(_) => {}
            _ => ast_visitor::walk_expr(self, expr),
        }
    }
}

/// The contract of `def`. `annotated` gives the columns of a schema named in an annotation.
pub(crate) fn derive(
    def: &ast::StmtFunctionDef,
    bound: bool,
    def_line: usize,
    annotated: &dyn Fn(&str) -> Option<Vec<String>>,
) -> FuncContract {
    let signature = Signature::of(def, bound);
    let mut requires = vec![Vec::new(); signature.len()];
    let loose_param = loose_first_param(def, annotated).and_then(|(name, columns)| {
        let index = (0..signature.len()).find(|&i| signature.name(i) == name)?;
        requires[index] = columns;
        Some(index)
    });
    let mut contract = FuncContract {
        signature,
        requires,
        def_line,
        loose_param,
        forwards: Vec::new(),
    };
    let names: Vec<String> = (0..contract.signature.len())
        .map(|i| contract.signature.name(i).to_string())
        .collect();
    let mut scan = CertainScan::new(&names);
    scan.body(&def.body);
    for (param, columns) in scan.reads.into_iter().enumerate() {
        contract.add_required(param, &columns);
    }
    contract.forwards = scan.forwards;
    contract
}

// The first parameter's body-wide contract: columns subscripted off it or a name derived
// from it, minus the names the body creates. Only a straight-line body counts, where every
// read runs on each call; a schema annotation replaces the scan.
fn loose_first_param(
    def: &ast::StmtFunctionDef,
    annotated: &dyn Fn(&str) -> Option<Vec<String>>,
) -> Option<(String, Vec<String>)> {
    let mut candidates = def
        .parameters
        .posonlyargs
        .iter()
        .chain(def.parameters.args.iter());
    let first = match candidates.next() {
        Some(p) if matches!(p.parameter.name.id.as_str(), "self" | "cls") => candidates.next(),
        other => other,
    }?;
    let name = first.parameter.name.id.to_string();
    let mut required = Vec::new();
    if is_straight_line(&def.body) {
        let mut tainted = HashSet::new();
        tainted.insert(name.clone());
        let mut delegates = Vec::new();
        for stmt in &def.body {
            contract::analyze_stmt_for_contract(stmt, &mut tainted, &mut required, &mut delegates);
        }
    }
    if let Some(columns) = first
        .parameter
        .annotation
        .as_ref()
        .and_then(|a| crate::ast_extract::extract_schema_from_annotation(a))
        .and_then(annotated)
    {
        required = columns;
    }
    let created = contract::created_names(&def.body);
    required.retain(|column| !created.contains(column));
    required.sort();
    required.dedup();
    Some((name, required))
}

/// Whether every statement of `body` runs on each call: simple statements only, a `return`
/// or `raise` last, and no expression that evaluates a part conditionally or later
/// (`a if c else b`, `and`/`or`, lambdas, comprehensions).
pub(crate) fn is_straight_line(body: &[Stmt]) -> bool {
    let mut conditional = Conditional(false);
    for (i, stmt) in body.iter().enumerate() {
        let last = i + 1 == body.len();
        match stmt {
            Stmt::Expr(_)
            | Stmt::Assign(_)
            | Stmt::AnnAssign(_)
            | Stmt::AugAssign(_)
            | Stmt::Pass(_)
            | Stmt::Assert(_)
            | Stmt::Import(_)
            | Stmt::ImportFrom(_) => {}
            Stmt::Return(_) | Stmt::Raise(_) if last => {}
            _ => return false,
        }
        conditional.visit_stmt(stmt);
        if conditional.0 {
            return false;
        }
    }
    true
}

struct Conditional(bool);

impl<'a> Visitor<'a> for Conditional {
    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::If(_)
            | Expr::BoolOp(_)
            | Expr::Lambda(_)
            | Expr::ListComp(_)
            | Expr::SetComp(_)
            | Expr::DictComp(_)
            | Expr::Generator(_) => self.0 = true,
            _ => ast_visitor::walk_expr(self, expr),
        }
    }
}

// Methods that change a frame's columns in place.
const IN_PLACE_METHODS: &[&str] = &[
    "insert",
    "pop",
    "pipe",
    "update",
    "insert_column",
    "drop_in_place",
    "replace_column",
    "hstack",
    "__setitem__",
    "__delitem__",
];

// Builtins a frame can be handed to without anything changing its columns.
const HARMLESS_CALLS: &[&str] = &["len", "print", "isinstance", "type", "id", "repr", "str"];

#[cfg(test)]
mod tests;

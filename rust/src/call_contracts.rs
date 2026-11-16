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
    let mut finder = Finder(false);
    for stmt in body {
        finder.visit_stmt(stmt);
    }
    finder.0
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

struct CertainScan {
    aliases: HashMap<String, usize>,
    done: Vec<bool>,
    reads: Vec<Vec<String>>,
    forwards: Vec<Forward>,
    // Per statement: names whose every occurrence was accounted for, and params to stop.
    accounted: HashSet<u32>,
    stop: Vec<usize>,
}

impl CertainScan {
    fn new(names: &[String]) -> Self {
        CertainScan {
            aliases: names
                .iter()
                .enumerate()
                .map(|(i, n)| (n.clone(), i))
                .collect(),
            done: vec![false; names.len()],
            reads: vec![Vec::new(); names.len()],
            forwards: Vec::new(),
            accounted: HashSet::new(),
            stop: Vec::new(),
        }
    }

    fn alias(&self, expr: &Expr) -> Option<usize> {
        let Expr::Name(name) = expr else {
            return None;
        };
        let param = *self.aliases.get(name.id.as_str())?;
        (!self.done[param]).then_some(param)
    }

    fn body(&mut self, body: &[Stmt]) {
        for stmt in body {
            if self.done.iter().all(|d| *d) || !self.statement(stmt) {
                return;
            }
        }
    }

    // Scans one top-level statement; `false` once later statements may not run.
    fn statement(&mut self, stmt: &Stmt) -> bool {
        self.accounted.clear();
        self.stop.clear();
        let mut mentions = Mentions {
            aliases: &self.aliases,
            found: Vec::new(),
        };
        mentions.visit_stmt(stmt);
        let found = mentions.found;
        let go_on = match stmt {
            Stmt::Expr(e) => {
                self.expr(&e.value);
                true
            }
            Stmt::Return(r) => {
                if let Some(value) = &r.value {
                    self.expr(value);
                }
                false
            }
            Stmt::Assign(assign) => {
                let forwarded = self.forwarded(&assign.value);
                self.expr(&assign.value);
                for target in &assign.targets {
                    self.target(target);
                }
                if let Some(param) = forwarded {
                    for target in &assign.targets {
                        if let Expr::Name(t) = target {
                            self.aliases.insert(t.id.to_string(), param);
                            self.done[param] = false;
                        }
                    }
                }
                true
            }
            Stmt::AnnAssign(assign) => {
                let forwarded = assign.value.as_deref().and_then(|v| self.forwarded(v));
                if let Some(value) = &assign.value {
                    self.expr(value);
                }
                self.target(&assign.target);
                if let (Expr::Name(t), Some(param)) = (&*assign.target, forwarded) {
                    self.aliases.insert(t.id.to_string(), param);
                    self.done[param] = false;
                }
                true
            }
            Stmt::AugAssign(assign) => {
                self.expr(&assign.value);
                if let Expr::Subscript(sub) = &*assign.target {
                    self.read(sub);
                }
                self.target(&assign.target);
                true
            }
            Stmt::If(if_stmt) => {
                self.expr(&if_stmt.test);
                false
            }
            Stmt::Assert(assert) => {
                self.expr(&assert.test);
                true
            }
            Stmt::Pass(_) => true,
            _ => false,
        };
        // Any other mention (a lambda, a container, a branch of a conditional) may let the
        // frame change in a way this scan doesn't follow.
        for (at, param) in found {
            if !self.accounted.contains(&at) {
                self.stop.push(param);
            }
        }
        for param in std::mem::take(&mut self.stop) {
            self.done[param] = true;
        }
        go_on
    }

    // `x = param` or `x = param.copy()`: `x` is the same frame (or a copy of it).
    fn forwarded(&mut self, value: &Expr) -> Option<usize> {
        let param = match value {
            Expr::Name(_) => self.alias(value),
            Expr::Call(call)
                if call.arguments.is_empty()
                    && matches!(&*call.func, Expr::Attribute(a) if a.attr.as_str() == "copy") =>
            {
                let Expr::Attribute(attr) = &*call.func else {
                    return None;
                };
                self.alias(&attr.value)
            }
            _ => None,
        };
        if param.is_some() {
            let at = match value {
                Expr::Call(call) => call.func.range().start(),
                _ => value.range().start(),
            };
            self.accounted.insert(at.to_u32());
        }
        param
    }

    // A store: rebinding a name ends its aliasing, writing through one ends its parameter.
    fn target(&mut self, target: &Expr) {
        match target {
            Expr::Name(name) => {
                if let Some(param) = self.aliases.remove(name.id.as_str()) {
                    if self.aliases.values().all(|p| *p != param) {
                        self.done[param] = true;
                    }
                }
            }
            Expr::Tuple(t) => t.elts.iter().for_each(|e| self.target(e)),
            Expr::List(l) => l.elts.iter().for_each(|e| self.target(e)),
            Expr::Starred(s) => self.target(&s.value),
            _ => {
                if let Some(param) = root_name(target).and_then(|n| self.aliases.get(n).copied()) {
                    self.done[param] = true;
                }
            }
        }
    }

    fn read(&mut self, sub: &ast::ExprSubscript) {
        let Some(param) = self.alias(&sub.value) else {
            return;
        };
        self.accounted.insert(sub.value.range().start().to_u32());
        if let Some(columns) = crate::ast_extract::extract_string_list_or_single(&sub.slice) {
            for column in columns {
                if !self.reads[param].contains(&column) {
                    self.reads[param].push(column);
                }
            }
        }
    }

    // Walks the parts of `expr` that are evaluated whenever it is.
    fn expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Subscript(sub) => {
                if sub.ctx == ast::ExprContext::Load {
                    self.read(sub);
                }
                self.expr(&sub.value);
                self.expr(&sub.slice);
            }
            Expr::Attribute(attr) => {
                if let Some(_param) = self.alias(&attr.value) {
                    self.accounted.insert(attr.value.range().start().to_u32());
                }
                self.expr(&attr.value);
            }
            Expr::Call(call) => self.call(call),
            Expr::BinOp(b) => {
                self.expr(&b.left);
                self.expr(&b.right);
            }
            Expr::UnaryOp(u) => self.expr(&u.operand),
            Expr::BoolOp(b) => {
                if let Some(first) = b.values.first() {
                    self.expr(first);
                }
            }
            Expr::Compare(c) => {
                self.expr(&c.left);
                for comparator in c.comparators.iter() {
                    self.expr(comparator);
                }
            }
            Expr::If(e) => self.expr(&e.test),
            Expr::List(l) => l.elts.iter().for_each(|e| self.expr(e)),
            Expr::Tuple(t) => t.elts.iter().for_each(|e| self.expr(e)),
            Expr::Name(_) => {
                // A bare alias here is one of the "other mentions" handled after the statement.
            }
            _ => {}
        }
    }

    fn polars_expression_receiver(&self, func: &Expr) -> Option<usize> {
        let Expr::Attribute(attr) = func else {
            return None;
        };
        match attr.attr.as_str() {
            "with_columns" | "select" | "filter" | "sort" | "group_by" => self.alias(&attr.value),
            "agg" => match &*attr.value {
                Expr::Call(grouped) => match &*grouped.func {
                    Expr::Attribute(g) if g.attr.as_str() == "group_by" => self.alias(&g.value),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        }
    }

    fn call(&mut self, call: &ast::ExprCall) {
        if let Expr::Attribute(attr) = &*call.func {
            if let Some(param) = self.alias(&attr.value) {
                let in_place = IN_PLACE_METHODS.contains(&attr.attr.as_str())
                    || call.arguments.keywords.iter().any(|k| {
                        k.arg
                            .as_ref()
                            .is_some_and(|a| matches!(a.as_str(), "inplace" | "in_place"))
                    });
                if in_place {
                    self.stop.push(param);
                }
            }
        }
        // `param.select(pl.col("a"))`, `param.group_by(..).agg(pl.col("a"))`: polars resolves
        // every column an expression names against `param`.
        if let Some(param) = self.polars_expression_receiver(&call.func) {
            let arguments = call
                .arguments
                .args
                .iter()
                .chain(call.arguments.keywords.iter().map(|k| &k.value));
            for column in arguments.flat_map(crate::ast_extract::collect_pl_col_names) {
                if !column.starts_with('^') && column != "*" && !self.reads[param].contains(&column)
                {
                    self.reads[param].push(column);
                }
            }
        }
        self.expr(&call.func);
        let harmless =
            matches!(&*call.func, Expr::Name(n) if HARMLESS_CALLS.contains(&n.id.as_str()));
        let shape = CallShape::of(call);
        let target = match &*call.func {
            Expr::Name(_) => Some(ForwardTarget::Call(call.func.range().start().to_u32())),
            Expr::Attribute(attr) => match &*attr.value {
                Expr::Name(base) if base.id.as_str() == "self" => {
                    Some(ForwardTarget::SelfMethod(attr.attr.to_string()))
                }
                Expr::Name(base) if !self.aliases.contains_key(base.id.as_str()) => {
                    Some(ForwardTarget::Call(call.func.range().start().to_u32()))
                }
                _ => None,
            },
            _ => None,
        };
        let arguments = call
            .arguments
            .args
            .iter()
            .chain(call.arguments.keywords.iter().map(|k| &k.value));
        for (index, argument) in arguments.enumerate() {
            if let Some(param) = self.alias(argument) {
                self.accounted.insert(argument.range().start().to_u32());
                if harmless {
                    continue;
                }
                // The callee may change the frame, so what follows is no longer certain.
                self.stop.push(param);
                if let (Some(target), Some(shape)) = (&target, &shape) {
                    self.forwards.push(Forward {
                        param,
                        target: target.clone(),
                        shape: shape.clone(),
                        argument: index,
                    });
                }
            } else {
                self.expr(argument);
            }
        }
    }
}

fn root_name(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Name(n) => Some(n.id.as_str()),
        Expr::Attribute(a) => root_name(&a.value),
        Expr::Subscript(s) => root_name(&s.value),
        Expr::Call(c) => root_name(&c.func),
        _ => None,
    }
}

struct Mentions<'m> {
    aliases: &'m HashMap<String, usize>,
    found: Vec<(u32, usize)>,
}

impl<'a> Visitor<'a> for Mentions<'_> {
    fn visit_expr(&mut self, expr: &'a Expr) {
        if let Expr::Name(name) = expr {
            if name.ctx == ast::ExprContext::Load {
                if let Some(param) = self.aliases.get(name.id.as_str()) {
                    self.found.push((name.range().start().to_u32(), *param));
                }
            }
        }
        ast_visitor::walk_expr(self, expr);
    }
}

/// The key a method or function is stored under in the project index.
pub(crate) fn index_key(key: &FnKey) -> String {
    match &key.0 {
        Some(class) => format!("{class}.{}", key.1),
        None => key.1.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruff_python_parser::parse_module;

    fn contract_of(source: &str) -> FuncContract {
        let module = parse_module(source).unwrap().into_syntax();
        let Stmt::FunctionDef(def) = &module.body[0] else {
            panic!("expected a function");
        };
        derive(def, false, 1, &|_| None)
    }

    #[test]
    fn test_should_require_reads_of_every_parameter_before_any_write() {
        let c = contract_of(
            "def f(a, b, *, c):\n    x = b[\"y\"] + c[[\"p\", \"q\"]]\n    b[\"z\"] = 1\n    return b[\"z\"] + a[\"k\"]\n",
        );
        assert_eq!(c.requires[1], vec!["y".to_string()]);
        assert_eq!(c.requires[2], vec!["p".to_string(), "q".to_string()]);
        assert_eq!(c.requires[0], vec!["k".to_string()]);
    }

    #[test]
    fn test_should_not_require_reads_after_a_branch_or_inside_one() {
        let c = contract_of(
            "def f(a, b):\n    if \"y\" in b.columns:\n        return b[\"y\"]\n    return b[\"z\"]\n",
        );
        assert!(c.requires[1].is_empty(), "{:?}", c.requires);
    }

    #[test]
    fn test_should_not_require_a_guarded_read_of_the_first_parameter() {
        let guarded = contract_of(
            "def maybe(df):\n    if \"a\" in df.columns:\n        return df[\"a\"]\n    return None\n",
        );
        let ternary = contract_of("def f(df):\n    return df[\"a\"] if \"a\" in df else None\n");
        let straight = contract_of("def f(df):\n    x = df[\"k\"] + 1\n    return df[\"v\"] * x\n");
        assert!(guarded.requires[0].is_empty(), "{:?}", guarded.requires);
        assert!(ternary.requires[0].is_empty(), "{:?}", ternary.requires);
        assert_eq!(straight.requires[0], vec!["k".to_string(), "v".to_string()]);
    }

    #[test]
    fn test_should_require_polars_columns_an_expression_names_on_the_parameter() {
        let c = contract_of(
            "def g(x, frame):\n    out = frame.group_by(\"d\").agg(pl.col(\"p\").sum())\n    return out\n",
        );
        assert_eq!(c.requires[1], vec!["p".to_string()]);
    }

    #[test]
    fn test_should_stop_at_a_call_that_may_change_the_frame() {
        let c = contract_of("def f(a, b):\n    add(b)\n    return b[\"new\"]\n");
        assert!(c.requires[1].is_empty());
        assert_eq!(c.forwards.len(), 1);
        assert_eq!(c.forwards[0].param, 1);
    }

    #[test]
    fn test_should_bind_keywords_and_reject_unknown_ones() {
        let sig = Signature {
            positional: vec!["a".into(), "b".into()],
            posonly: 0,
            kwonly: vec!["c".into()],
            varargs: false,
            kwargs: false,
        };
        let shape = CallShape {
            positional: 1,
            keywords: vec!["c".into(), "b".into()],
        };
        assert_eq!(sig.bind(&shape), Some(vec![Some(0), Some(2), Some(1)]));
        let bad = CallShape {
            positional: 1,
            keywords: vec!["a".into()],
        };
        assert_eq!(sig.bind(&bad), None);
        let extra = CallShape {
            positional: 3,
            keywords: vec![],
        };
        assert_eq!(sig.bind(&extra), None);
    }
}

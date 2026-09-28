//! Classifies the right-hand side of a reassignment relative to the DataFrames
//! the linter is tracking, so a name that gets rebound to something the checker
//! cannot model stops carrying its old schema.
//!
//! Pure functions over the AST -- no `Linter` state -- so the rules are testable in
//! isolation. The caller supplies which names are currently tracked frames.

use crate::ast_extract;
use crate::constants::{NON_FRAME_METHODS, ROW_PASSTHROUGH_METHODS, SCHEMA_PRESERVING_METHODS};
use ruff_python_ast::visitor::{self as ast_visitor, Visitor};
use ruff_python_ast::{self as ast, Expr, Stmt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A frame with the same columns as the tracked frame it derives from.
    Preserves,
    /// Probably a frame, but with columns the checker cannot determine.
    Derived,
    /// A Series, scalar, list, dict, or serialisation -- not a frame at all.
    NotAFrame,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Derivation {
    /// The tracked frame the expression derives from.
    pub(crate) root: String,
    pub(crate) kind: Kind,
}

/// `None` when the expression does not derive from any tracked frame.
pub(crate) fn classify_rhs(expr: &Expr, is_tracked: &dyn Fn(&str) -> bool) -> Option<Derivation> {
    match expr {
        Expr::Await(awaited) => classify_rhs(&awaited.value, is_tracked),
        Expr::Name(name) => is_tracked(name.id.as_str()).then(|| Derivation {
            root: name.id.to_string(),
            kind: Kind::Preserves,
        }),
        Expr::If(conditional) => {
            let derived = classify_rhs(&conditional.body, is_tracked)
                .or_else(|| classify_rhs(&conditional.orelse, is_tracked))?;
            Some(Derivation {
                root: derived.root,
                kind: Kind::Derived,
            })
        }
        Expr::Call(call) => classify_call(call, is_tracked),
        Expr::Subscript(subscript) => classify_subscript(subscript, is_tracked),
        Expr::Attribute(attr) => {
            let receiver = classify_rhs(&attr.value, is_tracked)?;
            let kind = if receiver.kind != Kind::NotAFrame && attr.attr.as_str() == "T" {
                Kind::Derived
            } else {
                Kind::NotAFrame
            };
            Some(Derivation {
                root: receiver.root,
                kind,
            })
        }
        _ => None,
    }
}

/// A short label for the operation an expression performs, for diagnostics
/// ("`clean()` is not modelled").
pub(crate) fn describe_rhs(expr: &Expr) -> String {
    match expr {
        Expr::Await(awaited) => describe_rhs(&awaited.value),
        Expr::Call(call) => match &*call.func {
            Expr::Attribute(attr) => format!("`.{}()`", attr.attr.as_str()),
            Expr::Name(name) => format!("`{}()`", name.id.as_str()),
            _ => "this call".to_string(),
        },
        Expr::Subscript(_) => "this subscript".to_string(),
        Expr::Attribute(attr) => format!("`.{}`", attr.attr.as_str()),
        _ => "this expression".to_string(),
    }
}

fn classify_call(call: &ast::ExprCall, is_tracked: &dyn Fn(&str) -> bool) -> Option<Derivation> {
    if let Expr::Attribute(attr) = &*call.func {
        if let Some(receiver) = classify_rhs(&attr.value, is_tracked) {
            let method = attr.attr.as_str();
            let kind = if receiver.kind == Kind::NotAFrame || NON_FRAME_METHODS.contains(&method) {
                Kind::NotAFrame
            } else if receiver.kind == Kind::Preserves
                && (ROW_PASSTHROUGH_METHODS.contains(&method)
                    || SCHEMA_PRESERVING_METHODS.contains(&method))
            {
                Kind::Preserves
            } else {
                Kind::Derived
            };
            return Some(Derivation {
                root: receiver.root,
                kind,
            });
        }
    }
    first_arg_root(call, is_tracked).map(|root| Derivation {
        root,
        kind: Kind::Derived,
    })
}

/// One change a helper makes to the columns of the frame it is handed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum ColumnEdit {
    Add(Vec<String>),
    Remove(Vec<String>),
    Rename(HashMap<String, String>),
    Replace(Vec<String>),
    /// The helper hands the frame to another helper (`_fix(df)`), whose own in-place
    /// edits apply here -- replaced by `resolve_delegates` once every summary is known.
    /// `conditional` when the call sits under a branch or after a conditional rebind.
    Delegate {
        callee: FnRef,
        conditional: bool,
    },
    /// The helper returns what another helper returns for the same frame
    /// (`return _clean(df)`): the callee's own returned-frame edits apply.
    ReturnedFrom(FnRef),
    Unknown,
}

/// A function or method by its defining class (`None` at module level or nested in a
/// function) and name.
pub(crate) type FnKey = (Option<String>, String);

/// A callee named at a call site: `name(...)`, or `self.name(...)`/`cls.name(...)` when
/// `via_self` (a method of the caller's own class).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct FnRef {
    pub(crate) via_self: bool,
    /// `Some(alias)` for `alias.name(...)` where `alias` is a plainly-imported module
    /// (`import helpers` / `import helpers as alias`). Mutually exclusive with `via_self`.
    pub(crate) module: Option<String>,
    pub(crate) name: String,
}

/// The column set after `edits`, or `None` when any edit is not understood.
pub(crate) fn apply_edits(base: &[String], edits: &[ColumnEdit]) -> Option<Vec<String>> {
    let mut columns = base.to_vec();
    for edit in edits {
        match edit {
            ColumnEdit::Add(added) => {
                for column in added {
                    if !columns.contains(column) {
                        columns.push(column.clone());
                    }
                }
            }
            ColumnEdit::Remove(removed) => columns.retain(|c| !removed.contains(c)),
            ColumnEdit::Rename(mapping) => {
                for column in &mut columns {
                    if let Some(new_name) = mapping.get(column) {
                        *column = new_name.clone();
                    }
                }
            }
            ColumnEdit::Replace(replacement) => columns = replacement.clone(),
            ColumnEdit::Delegate { .. } | ColumnEdit::ReturnedFrom(_) | ColumnEdit::Unknown => {
                return None;
            }
        }
    }
    Some(columns)
}

/// `class` followed by its ancestors in `bases`, nearest first.
pub(crate) fn class_chain(bases: &HashMap<String, Vec<String>>, class: &str) -> Vec<String> {
    let mut chain: Vec<String> = Vec::new();
    let mut pending = vec![class.to_string()];
    while let Some(next) = pending.pop() {
        if chain.contains(&next) {
            continue;
        }
        if let Some(parents) = bases.get(&next) {
            pending.extend(parents.iter().rev().cloned());
        }
        chain.push(next);
    }
    chain
}

/// Replaces every `Delegate` in `summaries` with the callee's own in-place edits. A
/// callee with no summary (or none in place) contributes nothing; a conditional call to
/// one that edits, and any cycle of delegation, is `Unknown`.
///
/// `preserve_unresolved` is for index-time use only (see `Linter::preserve_unresolved_delegates`):
/// a call to a name this file does not itself define may still be a cross-file helper the
/// project index hasn't resolved yet, so the raw `Delegate`/`ReturnedFrom` is kept rather
/// than dropped, for `resolve_transitive_helper_summaries` to finish later. Never set at
/// check time, where every name the project can resolve is already merged in, so "not
/// found" there really does mean "no effect" -- see that call site.
pub(crate) fn resolve_delegates(
    summaries: &mut HashMap<FnKey, HelperSummary>,
    bases: &HashMap<String, Vec<String>>,
    preserve_unresolved: bool,
) {
    struct Resolver<'a> {
        original: &'a HashMap<FnKey, HelperSummary>,
        bases: &'a HashMap<String, Vec<String>>,
        stack: Vec<FnKey>,
        preserve_unresolved: bool,
    }

    impl<'a> Resolver<'a> {
        // The summary `callee` names when called from `owner`'s code: a plain function, or
        // a method of `owner` or one it inherits.
        fn lookup(
            &self,
            callee: &FnRef,
            owner: &Option<String>,
        ) -> Option<(FnKey, &'a HelperSummary)> {
            if callee.via_self {
                return class_chain(self.bases, owner.as_ref()?)
                    .into_iter()
                    .map(|class| (Some(class), callee.name.clone()))
                    .find_map(|key| self.original.get(&key).map(|s| (key, s)));
            }
            // `imported_summaries` (merged into `original` before this runs -- see
            // `Linter::helper_summaries`) keys a module-qualified import the same way.
            let class_slot = callee
                .module
                .as_ref()
                .map(|alias| format!("module:{alias}"));
            let key = (class_slot, callee.name.clone());
            self.original.get(&key).map(|s| (key, s))
        }

        fn expand(&mut self, edits: &[ColumnEdit], owner: &Option<String>) -> Vec<ColumnEdit> {
            let mut out = Vec::new();
            for edit in edits {
                let ColumnEdit::Delegate {
                    callee,
                    conditional,
                } = edit
                else {
                    out.push(edit.clone());
                    continue;
                };
                let Some((key, summary)) = self.lookup(callee, owner) else {
                    if self.preserve_unresolved {
                        out.push(edit.clone());
                    }
                    continue;
                };
                if self.stack.contains(&key) {
                    out.push(ColumnEdit::Unknown);
                    continue;
                }
                self.stack.push(key.clone());
                let callee_edits = self.expand(&summary.in_place, &key.0);
                self.stack.pop();
                if callee_edits.is_empty() {
                    continue;
                }
                if *conditional {
                    out.push(ColumnEdit::Unknown);
                } else {
                    out.extend(callee_edits);
                }
            }
            out
        }

        // The edits a helper's returned frame ends up with, or `None` when it returns
        // another helper's result and that one is not itself a known pass-through (or is
        // a cycle).
        fn expand_returned(
            &mut self,
            edits: &[ColumnEdit],
            owner: &Option<String>,
        ) -> Option<Vec<ColumnEdit>> {
            let mut out = Vec::new();
            for edit in edits {
                let ColumnEdit::ReturnedFrom(callee) = edit else {
                    out.extend(self.expand(std::slice::from_ref(edit), owner));
                    continue;
                };
                let Some((key, summary)) = self.lookup(callee, owner) else {
                    if self.preserve_unresolved {
                        out.push(edit.clone());
                        continue;
                    }
                    return None;
                };
                if self.stack.contains(&key) {
                    if self.preserve_unresolved {
                        out.push(edit.clone());
                        continue;
                    }
                    return None;
                }
                let Some(returned) = &summary.returned else {
                    if self.preserve_unresolved {
                        out.push(edit.clone());
                        continue;
                    }
                    return None;
                };
                self.stack.push(key.clone());
                let expanded = self.expand_returned(returned, &key.0);
                self.stack.pop();
                match expanded {
                    Some(expanded) => out.extend(expanded),
                    None if self.preserve_unresolved => out.push(edit.clone()),
                    None => return None,
                }
            }
            Some(out)
        }
    }

    let original = summaries.clone();
    for (key, summary) in summaries.iter_mut() {
        let mut resolver = Resolver {
            original: &original,
            bases,
            stack: vec![key.clone()],
            preserve_unresolved,
        };
        summary.in_place = resolver.expand(&summary.in_place, &key.0);
        if let Some(returned) = &summary.returned {
            summary.returned = resolver.expand_returned(returned, &key.0);
        }
    }
    summaries.retain(|_, s| s.returned.is_some() || !s.in_place.is_empty());
}

/// What a call `f(df, ...)` does, given `f`'s definition and its first parameter (after
/// `self`/`cls`). Purely syntactic, so it can be computed for every function in a file
/// before the walk, wherever `f` is defined.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct HelperSummary {
    /// Set when `f` hands its first parameter's frame straight back -- every `return` is a
    /// schema-preserving derivation of it, and so is every reassignment of it: the column
    /// edits made to the returned frame (empty for a pure pass-through).
    pub(crate) returned: Option<Vec<ColumnEdit>>,
    /// Edits that reach the caller's own object because they happen before the parameter
    /// is rebound (`df["z"] = 1` in place, as opposed to on a `df = df.copy()`).
    pub(crate) in_place: Vec<ColumnEdit>,
}

/// An edit that only happens under a condition or loop is `Unknown`: the frame's columns
/// then depend on a branch the checker does not follow. `None` when `f` neither returns
/// its parameter nor edits it in place -- a call to it says nothing about the frame.
pub(crate) fn summarize_helper(func_def: &ast::StmtFunctionDef) -> Option<HelperSummary> {
    let parameters = &func_def.parameters;
    let mut names = parameters
        .posonlyargs
        .iter()
        .chain(parameters.args.iter())
        .map(|p| p.parameter.name.as_str());
    let mut first = names.next()?;
    if matches!(first, "self" | "cls") {
        first = names.next()?;
    }
    let mut scan = PassthroughScan {
        param: first,
        saw_return: false,
        ok: true,
        depth: 0,
        alias: Alias::Yes,
        edits: Vec::new(),
        in_place: Vec::new(),
    };
    for stmt in &func_def.body {
        scan.visit_stmt(stmt);
    }
    let returned = (scan.saw_return && scan.ok).then_some(scan.edits);
    (returned.is_some() || !scan.in_place.is_empty()).then_some(HelperSummary {
        returned,
        in_place: scan.in_place,
    })
}

fn preserves_param(expr: &Expr, param: &str) -> bool {
    classify_rhs(expr, &|name| name == param)
        .is_some_and(|derived| derived.root == param && derived.kind == Kind::Preserves)
}

// Whether the parameter still names the caller's own object.
#[derive(Clone, Copy, PartialEq)]
enum Alias {
    Yes,
    // Rebound under a condition: later edits may or may not reach the caller.
    Maybe,
    No,
}

struct PassthroughScan<'p> {
    param: &'p str,
    saw_return: bool,
    ok: bool,
    // Nesting inside if/for/while/try/with/match.
    depth: u32,
    alias: Alias,
    // Every edit, in order (what the returned frame ends up with).
    edits: Vec<ColumnEdit>,
    // The subset that reaches the caller's object.
    in_place: Vec<ColumnEdit>,
}

impl PassthroughScan<'_> {
    fn names_param(&self, target: &Expr) -> bool {
        match target {
            Expr::Name(name) => name.id.as_str() == self.param,
            Expr::Tuple(tuple) => tuple.elts.iter().any(|e| self.names_param(e)),
            Expr::List(list) => list.elts.iter().any(|e| self.names_param(e)),
            Expr::Starred(starred) => self.names_param(&starred.value),
            _ => false,
        }
    }

    fn is_param(&self, expr: &Expr) -> bool {
        matches!(expr, Expr::Name(name) if name.id.as_str() == self.param)
    }

    fn record(&mut self, edit: ColumnEdit) {
        let edit = if self.depth > 0 {
            ColumnEdit::Unknown
        } else {
            edit
        };
        match self.alias {
            Alias::Yes => self.in_place.push(edit.clone()),
            Alias::Maybe => self.in_place.push(ColumnEdit::Unknown),
            Alias::No => {}
        }
        self.edits.push(edit);
    }

    // The parameter now names something else (a copy, a derived frame).
    fn rebind(&mut self) {
        if self.alias != Alias::No {
            self.alias = if self.depth > 0 {
                Alias::Maybe
            } else {
                Alias::No
            };
        }
    }

    fn record_assignment_edits(&mut self, target: &Expr, value: &Expr) {
        if let Some((recv, columns)) = column_write_target(target) {
            if recv == self.param {
                self.record(ColumnEdit::Add(columns));
            }
        } else if let Expr::Subscript(subscript) = target {
            // `df[name] = ...` may add a column of unknown name; a comparison or slice is
            // a row selection and adds none.
            if self.is_param(&subscript.value)
                && !matches!(
                    &*subscript.slice,
                    Expr::Compare(_) | Expr::BoolOp(_) | Expr::UnaryOp(_) | Expr::Slice(_)
                )
            {
                self.record(ColumnEdit::Unknown);
            }
        } else if let Expr::Attribute(attr) = target {
            if attr.attr.as_str() == "columns" && self.is_param(&attr.value) {
                self.record(
                    ast_extract::extract_string_list(value)
                        .map_or(ColumnEdit::Unknown, ColumnEdit::Replace),
                );
            }
        }
    }

    fn record_delete_edit(&mut self, target: &Expr) {
        let Expr::Subscript(subscript) = target else {
            return;
        };
        if !self.is_param(&subscript.value) {
            return;
        }
        self.record(
            ast_extract::extract_string_literal(&subscript.slice)
                .map_or(ColumnEdit::Unknown, |c| {
                    ColumnEdit::Remove(vec![c.to_string()])
                }),
        );
    }

    // `g(param, ...)` / `self.g(param, ...)`: the helper being handed the parameter.
    fn callee_handed_param(&self, call: &ast::ExprCall) -> Option<FnRef> {
        let callee = match &*call.func {
            Expr::Name(name) if name.id.as_str() != self.param => FnRef {
                via_self: false,
                module: None,
                name: name.id.to_string(),
            },
            Expr::Attribute(attr) => match &*attr.value {
                Expr::Name(receiver) if matches!(receiver.id.as_str(), "self" | "cls") => FnRef {
                    via_self: true,
                    module: None,
                    name: attr.attr.to_string(),
                },
                // `helpers.fix(param)` for a plainly-imported module -- excludes the
                // parameter's own name, which is a method call ON the frame
                // (`record_call_edit` already covers that), not a delegate to it.
                Expr::Name(module) if module.id.as_str() != self.param => FnRef {
                    via_self: false,
                    module: Some(module.id.to_string()),
                    name: attr.attr.to_string(),
                },
                _ => return None,
            },
            _ => return None,
        };
        call.arguments
            .args
            .first()
            .is_some_and(|a| self.is_param(a))
            .then_some(callee)
    }

    // `g(param)` / `await self.g(param)`: the helper being handed the parameter.
    fn handed_on(&self, expr: &Expr) -> Option<FnRef> {
        let mut expr = expr;
        while let Expr::Await(awaited) = expr {
            expr = &awaited.value;
        }
        match expr {
            Expr::Call(call) => self.callee_handed_param(call),
            _ => None,
        }
    }

    // The in-place effect of handing the parameter to `callee`; `returned` when the call's
    // result is what this function returns (`return g(param)`).
    fn record_delegate(&mut self, callee: FnRef, returned: bool) {
        let make = |conditional| ColumnEdit::Delegate {
            callee: callee.clone(),
            conditional,
        };
        match self.alias {
            Alias::Yes => self.in_place.push(make(self.depth > 0)),
            Alias::Maybe => self.in_place.push(make(true)),
            Alias::No => {}
        }
        self.edits.push(if returned {
            ColumnEdit::ReturnedFrom(callee)
        } else {
            make(self.depth > 0)
        });
    }

    fn record_call_edit(&mut self, call: &ast::ExprCall) {
        let Expr::Attribute(attr) = &*call.func else {
            return;
        };
        if !self.is_param(&attr.value) {
            return;
        }
        let method = attr.attr.as_str();
        let first_literal = |index: usize| {
            call.arguments
                .args
                .get(index)
                .and_then(|a| ast_extract::extract_string_literal(a))
                .map(str::to_string)
        };
        let edit = match method {
            "pop" => {
                Some(first_literal(0).map_or(ColumnEdit::Unknown, |c| ColumnEdit::Remove(vec![c])))
            }
            "insert" => {
                Some(first_literal(1).map_or(ColumnEdit::Unknown, |c| ColumnEdit::Add(vec![c])))
            }
            _ if keyword(call, "inplace").is_none() => None,
            _ => match inplace_literal(call) {
                Some(false) => None,
                Some(true)
                    if ROW_PASSTHROUGH_METHODS.contains(&method)
                        || SCHEMA_PRESERVING_METHODS.contains(&method) =>
                {
                    None
                }
                Some(true) => match method {
                    "drop" => match inplace_drop_columns(call) {
                        InplaceColumns::Edit(dropped) => Some(ColumnEdit::Remove(dropped)),
                        InplaceColumns::Unchanged => None,
                        InplaceColumns::Unknown => Some(ColumnEdit::Unknown),
                    },
                    "rename" => match inplace_rename_mapping(call) {
                        InplaceColumns::Edit(mapping) => Some(ColumnEdit::Rename(mapping)),
                        InplaceColumns::Unchanged => None,
                        InplaceColumns::Unknown => Some(ColumnEdit::Unknown),
                    },
                    _ => Some(ColumnEdit::Unknown),
                },
                None => Some(ColumnEdit::Unknown),
            },
        };
        if let Some(edit) = edit {
            self.record(edit);
        }
    }
}

impl<'b> Visitor<'b> for PassthroughScan<'_> {
    fn visit_stmt(&mut self, stmt: &'b Stmt) {
        match stmt {
            // A nested scope has its own returns and its own bindings.
            Stmt::FunctionDef(_) | Stmt::ClassDef(_) => return,
            Stmt::Return(ret) => {
                self.saw_return = true;
                let value = ret.value.as_deref();
                if let Some(callee) = value.and_then(|v| self.handed_on(v)) {
                    self.record_delegate(callee, true);
                } else if !value.is_some_and(|value| preserves_param(value, self.param)) {
                    self.ok = false;
                }
            }
            Stmt::Assign(assign) => {
                // `param = g(param)`: the helper's result replaces the parameter. It may
                // be the same object or a copy, so later edits are only possibly in place.
                if let ([target], Some(callee)) =
                    (assign.targets.as_slice(), self.handed_on(&assign.value))
                {
                    if self.is_param(target) {
                        self.record_delegate(callee, true);
                        if self.alias != Alias::No {
                            self.alias = Alias::Maybe;
                        }
                        return;
                    }
                }
                for target in &assign.targets {
                    if self.names_param(target) {
                        if !(matches!(target, Expr::Name(_))
                            && preserves_param(&assign.value, self.param))
                        {
                            self.ok = false;
                        }
                        self.rebind();
                    }
                    self.record_assignment_edits(target, &assign.value);
                }
            }
            Stmt::AnnAssign(ann) if self.names_param(&ann.target) => {
                if !ann
                    .value
                    .as_deref()
                    .is_some_and(|value| preserves_param(value, self.param))
                {
                    self.ok = false;
                }
                self.rebind();
            }
            Stmt::For(for_stmt) if self.names_param(&for_stmt.target) => {
                self.ok = false;
                self.rebind();
            }
            Stmt::With(with_stmt)
                if with_stmt.items.iter().any(|item| {
                    item.optional_vars
                        .as_deref()
                        .is_some_and(|v| self.names_param(v))
                }) =>
            {
                self.ok = false;
                self.rebind();
            }
            Stmt::Delete(delete) => {
                for target in &delete.targets {
                    if self.names_param(target) {
                        self.ok = false;
                        self.rebind();
                    }
                    self.record_delete_edit(target);
                }
            }
            Stmt::Expr(expr_stmt) => {
                let mut value = &*expr_stmt.value;
                while let Expr::Await(awaited) = value {
                    value = &awaited.value;
                }
                if let Expr::Call(call) = value {
                    self.record_call_edit(call);
                    if let Some(callee) = self.callee_handed_param(call) {
                        self.record_delegate(callee, false);
                    }
                }
            }
            Stmt::If(_)
            | Stmt::For(_)
            | Stmt::While(_)
            | Stmt::Try(_)
            | Stmt::With(_)
            | Stmt::Match(_) => {
                self.depth += 1;
                ast_visitor::walk_stmt(self, stmt);
                self.depth -= 1;
                return;
            }
            _ => {}
        }
        ast_visitor::walk_stmt(self, stmt);
    }

    fn visit_expr(&mut self, expr: &'b Expr) {
        if let Expr::Named(named) = expr {
            if self.names_param(&named.target) {
                self.ok = false;
                self.rebind();
            }
        }
        ast_visitor::walk_expr(self, expr);
    }
}

// `f(df, ...)`, `module.f(df, ...)`, `pd.concat([df, other])`: the result's columns
// are whatever the callee produces. A first argument that is itself not a frame
// (`f(df["a"])`) says nothing about a frame result, so it does not count.
fn first_arg_root(call: &ast::ExprCall, is_tracked: &dyn Fn(&str) -> bool) -> Option<String> {
    let tracked_name = |expr: &Expr| match expr {
        Expr::Name(name) if is_tracked(name.id.as_str()) => Some(name.id.to_string()),
        _ => None,
    };
    let first = call.arguments.args.first()?;
    match first {
        Expr::List(list) => list.elts.iter().find_map(tracked_name),
        Expr::Tuple(tuple) => tuple.elts.iter().find_map(tracked_name),
        other => classify_rhs(other, is_tracked)
            .filter(|derived| derived.kind != Kind::NotAFrame)
            .map(|derived| derived.root),
    }
}

fn classify_subscript(
    subscript: &ast::ExprSubscript,
    is_tracked: &dyn Fn(&str) -> bool,
) -> Option<Derivation> {
    if let Expr::Attribute(indexer) = &*subscript.value {
        let name = indexer.attr.as_str();
        if matches!(name, "loc" | "iloc" | "at" | "iat") {
            let receiver = classify_rhs(&indexer.value, is_tracked)?;
            let kind = if receiver.kind == Kind::NotAFrame || matches!(name, "at" | "iat") {
                Kind::NotAFrame
            } else {
                match (indexer_kind(&subscript.slice), receiver.kind) {
                    (Kind::Preserves, Kind::Derived) => Kind::Derived,
                    (kind, _) => kind,
                }
            };
            return Some(Derivation {
                root: receiver.root,
                kind,
            });
        }
    }

    let receiver = classify_rhs(&subscript.value, is_tracked)?;
    if receiver.kind == Kind::NotAFrame {
        return Some(receiver);
    }
    let kind = match &*subscript.slice {
        Expr::StringLiteral(_) => Kind::NotAFrame,
        Expr::List(_) => Kind::Derived,
        // A boolean mask or row slice keeps the columns; any other slice expression is
        // treated the same way, matching how the assignment dispatch already handles
        // `df[<not a literal list>]`.
        _ if receiver.kind == Kind::Preserves => Kind::Preserves,
        _ => Kind::Derived,
    };
    Some(Derivation {
        root: receiver.root,
        kind,
    })
}

// `.loc[...]` / `.iloc[...]`: a single indexer selects rows and keeps every column;
// `(rows, cols)` keeps them only when `cols` is a bare `:`.
fn indexer_kind(slice: &Expr) -> Kind {
    let scalar = |expr: &Expr| matches!(expr, Expr::StringLiteral(_) | Expr::NumberLiteral(_));
    match slice {
        Expr::Tuple(tuple) if tuple.elts.len() == 2 => match &tuple.elts[1] {
            Expr::Slice(full)
                if full.lower.is_none() && full.upper.is_none() && full.step.is_none() =>
            {
                if scalar(&tuple.elts[0]) {
                    Kind::NotAFrame
                } else {
                    Kind::Preserves
                }
            }
            column if scalar(column) => Kind::NotAFrame,
            _ => Kind::Derived,
        },
        rows if scalar(rows) => Kind::NotAFrame,
        _ => Kind::Preserves,
    }
}

// The receiver and columns of a subscript write into a frame: `df["c"] = ...`,
// `df[["a", "b"]] = ...`, `df.loc[rows, "c"] = ...`, `df.loc[rows, ["a", "b"]] = ...`.
pub(crate) fn column_write_target(target: &Expr) -> Option<(String, Vec<String>)> {
    let Expr::Subscript(subscript) = target else {
        return None;
    };
    let columns_of = |expr: &Expr| {
        ast_extract::extract_string_literal(expr)
            .map(|c| vec![c.to_string()])
            .or_else(|| ast_extract::extract_string_list(expr))
    };
    match &*subscript.value {
        Expr::Name(recv) => Some((recv.id.to_string(), columns_of(&subscript.slice)?)),
        Expr::Attribute(indexer) if indexer.attr.as_str() == "loc" => {
            let Expr::Name(recv) = &*indexer.value else {
                return None;
            };
            let Expr::Tuple(indexers) = &*subscript.slice else {
                return None;
            };
            let [_, columns] = indexers.elts.as_slice() else {
                return None;
            };
            Some((recv.id.to_string(), columns_of(columns)?))
        }
        _ => None,
    }
}

pub(crate) fn keyword<'c>(call: &'c ast::ExprCall, name: &str) -> Option<&'c ast::Keyword> {
    call.arguments
        .keywords
        .iter()
        .find(|k| k.arg.as_ref().map(|a| a.as_str()) == Some(name))
}

// The literal value of `inplace=`, or `None` when it is absent or not a literal.
pub(crate) fn inplace_literal(call: &ast::ExprCall) -> Option<bool> {
    match &keyword(call, "inplace")?.value {
        Expr::BooleanLiteral(b) => Some(b.value),
        _ => None,
    }
}

// What an in-place `drop`/`rename` does to a frame's columns.
pub(crate) enum InplaceColumns<T> {
    Edit(T),
    Unchanged,
    Unknown,
}

pub(crate) enum Axis {
    Unspecified,
    Rows,
    Columns,
    Dynamic,
}

pub(crate) fn axis_of(call: &ast::ExprCall) -> Axis {
    let Some(axis) = keyword(call, "axis") else {
        return Axis::Unspecified;
    };
    let is = |int: u64, name: &str| match &axis.value {
        Expr::NumberLiteral(n) => {
            matches!(&n.value, ast::Number::Int(i) if i.as_u64() == Some(int))
        }
        Expr::StringLiteral(s) => s.value.to_str() == name,
        _ => false,
    };
    if is(0, "index") {
        Axis::Rows
    } else if is(1, "columns") {
        Axis::Columns
    } else {
        Axis::Dynamic
    }
}

// `inplace=` only exists in pandas, where `drop`'s positional labels are rows unless
// `axis=1`/`columns=` says otherwise.
pub(crate) fn inplace_drop_columns(call: &ast::ExprCall) -> InplaceColumns<Vec<String>> {
    let from = |expr: &Expr| {
        ast_extract::extract_string_list_or_single(expr)
            .map_or(InplaceColumns::Unknown, InplaceColumns::Edit)
    };
    if let Some(columns) = keyword(call, "columns") {
        return from(&columns.value);
    }
    match axis_of(call) {
        Axis::Unspecified | Axis::Rows => InplaceColumns::Unchanged,
        Axis::Columns => call
            .arguments
            .args
            .first()
            .or_else(|| keyword(call, "labels").map(|k| &k.value))
            .map_or(InplaceColumns::Unknown, from),
        Axis::Dynamic => InplaceColumns::Unknown,
    }
}

// As for `drop`, a positional mapper renames row labels unless `axis=1`/`columns=` says
// otherwise.
pub(crate) fn inplace_rename_mapping(
    call: &ast::ExprCall,
) -> InplaceColumns<HashMap<String, String>> {
    let from = |expr: &Expr| match expr {
        Expr::Dict(dict) => ast_extract::extract_string_dict(dict)
            .map_or(InplaceColumns::Unknown, InplaceColumns::Edit),
        _ => InplaceColumns::Unknown,
    };
    if let Some(columns) = keyword(call, "columns") {
        return from(&columns.value);
    }
    match axis_of(call) {
        Axis::Unspecified | Axis::Rows => InplaceColumns::Unchanged,
        Axis::Columns => call
            .arguments
            .args
            .first()
            .or_else(|| keyword(call, "mapper").map(|k| &k.value))
            .map_or(InplaceColumns::Unknown, from),
        Axis::Dynamic => InplaceColumns::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruff_python_ast::Stmt;
    use ruff_python_parser::parse_module;

    fn classify(rhs: &str) -> Option<Derivation> {
        let source = format!("x = {rhs}\n");
        let module = parse_module(&source).unwrap().into_syntax();
        let Stmt::Assign(assign) = &module.body[0] else {
            panic!("expected an assignment");
        };
        classify_rhs(&assign.value, &|name| name == "df" || name == "other")
    }

    fn kind(rhs: &str) -> Option<(String, Kind)> {
        classify(rhs).map(|derived| (derived.root, derived.kind))
    }

    fn df(kind: Kind) -> Option<(String, Kind)> {
        Some(("df".to_string(), kind))
    }

    #[test]
    fn test_should_treat_schema_preserving_methods_as_preserving() {
        for rhs in [
            "df.copy()",
            "df.drop_duplicates()",
            "df.astype({'a': 'int'})",
            "df.head().reset_index()",
            "df.copy().sort_values('a').head(3)",
        ] {
            assert_eq!(kind(rhs), df(Kind::Preserves), "{rhs}");
        }
    }

    #[test]
    fn test_should_treat_unknown_methods_and_functions_as_derived() {
        for rhs in [
            "df.some_unknown()",
            "df.groupby('a').agg({'b': 'sum'})",
            "df.head().some_unknown()",
            "clean(df)",
            "module.clean(df, 3)",
            "pd.concat([df, new_rows])",
            "df.T",
            "df if cond else other_thing()",
        ] {
            assert_eq!(kind(rhs), df(Kind::Derived), "{rhs}");
        }
    }

    #[test]
    fn test_should_treat_series_and_scalar_results_as_not_a_frame() {
        for rhs in [
            "df.sum()",
            "df['a']",
            "df['a'].map(f)",
            "df.columns",
            "df.to_dict()",
            "df.loc[:, 'a']",
            "df.iloc[0]",
            "df.at[0, 'a']",
        ] {
            assert_eq!(kind(rhs), df(Kind::NotAFrame), "{rhs}");
        }
    }

    #[test]
    fn test_should_keep_columns_for_row_selections() {
        for rhs in [
            "df[df['a'] > 1]",
            "df[mask]",
            "df.loc[df['a'] > 1]",
            "df.loc[mask, :]",
            "df.iloc[:10]",
            "df.head()[mask]",
        ] {
            assert_eq!(kind(rhs), df(Kind::Preserves), "{rhs}");
        }
    }

    #[test]
    fn test_should_treat_column_selections_as_derived() {
        for rhs in ["df.loc[:, ['a', 'b']]", "df.head()[['a', 'b']]"] {
            assert_eq!(kind(rhs), df(Kind::Derived), "{rhs}");
        }
    }

    #[test]
    fn test_should_not_derive_from_untracked_names() {
        for rhs in ["load_other()", "untracked.copy()", "f(untracked)", "1 + 2"] {
            assert_eq!(kind(rhs), None, "{rhs}");
        }
    }

    #[test]
    fn test_should_ignore_a_non_frame_first_argument() {
        assert_eq!(kind("f(df['a'])"), None);
    }

    #[test]
    fn test_should_describe_the_operation_for_diagnostics() {
        let describe = |rhs: &str| {
            let source = format!("x = {rhs}\n");
            let module = parse_module(&source).unwrap().into_syntax();
            let Stmt::Assign(assign) = &module.body[0] else {
                panic!("expected an assignment");
            };
            describe_rhs(&assign.value)
        };
        assert_eq!(describe("clean(df)"), "`clean()`");
        assert_eq!(describe("df.groupby('a').agg(f)"), "`.agg()`");
        assert_eq!(describe("await fetch(df)"), "`fetch()`");
        assert_eq!(describe("df['a']"), "this subscript");
        assert_eq!(describe("df.T"), "`.T`");
        assert_eq!(describe("(lambda: 1)()"), "this call");
        assert_eq!(describe("a + b"), "this expression");
    }

    fn passthrough(source: &str) -> bool {
        let module = parse_module(source).unwrap().into_syntax();
        let Stmt::FunctionDef(func_def) = &module.body[0] else {
            panic!("expected a function");
        };
        summarize_helper(func_def).is_some_and(|s| s.returned.is_some())
    }

    #[test]
    fn test_should_infer_a_helper_that_returns_its_frame_unchanged() {
        assert!(passthrough(
            "def clean(df):\n    return df.dropna().reset_index()\n"
        ));
        assert!(passthrough(
            "def clean(df):\n    df = df.copy()\n    df = df.head(3)\n    return df\n"
        ));
        assert!(passthrough(
            "def clean(self, df):\n    if x:\n        return df\n    return df.copy()\n"
        ));
    }

    #[test]
    fn test_should_not_infer_a_helper_that_changes_or_drops_the_frame() {
        for body in [
            "return df.groupby('a').agg(f)",
            "return other",
            "return",
            "pass",
            "df = df.merge(x)\n    return df",
            "df, y = split(df)\n    return df",
            "for df in dfs:\n        pass\n    return df",
            "return df['a']",
            "df: object = other\n    return df",
            "with opener() as df:\n        pass\n    return df",
            "del df\n    return df",
            "(df := other)\n    return df",
        ] {
            let source = format!("def clean(df):\n    {body}\n");
            assert!(!passthrough(&source), "{body}");
        }
        assert!(!passthrough("def clean():\n    return 1\n"));
    }

    fn edits(source: &str) -> Option<Vec<ColumnEdit>> {
        let module = parse_module(source).unwrap().into_syntax();
        let Stmt::FunctionDef(func_def) = &module.body[0] else {
            panic!("expected a function");
        };
        summarize_helper(func_def).and_then(|s| s.returned)
    }

    fn strings(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    fn in_place(source: &str) -> Option<Vec<ColumnEdit>> {
        let module = parse_module(source).unwrap().into_syntax();
        let Stmt::FunctionDef(func_def) = &module.body[0] else {
            panic!("expected a function");
        };
        summarize_helper(func_def).map(|s| s.in_place)
    }

    #[test]
    fn test_should_only_count_edits_made_before_the_parameter_is_rebound() {
        let before =
            "def f(df):\n    df['a'] = 1\n    df = df.copy()\n    df['b'] = 2\n    return df\n";
        assert_eq!(
            in_place(before),
            Some(vec![ColumnEdit::Add(strings(&["a"]))])
        );
        assert_eq!(
            edits(before),
            Some(vec![
                ColumnEdit::Add(strings(&["a"])),
                ColumnEdit::Add(strings(&["b"]))
            ])
        );
        assert_eq!(
            in_place("def f(df):\n    df = df.copy()\n    df['b'] = 2\n    return df\n"),
            Some(Vec::new())
        );
    }

    #[test]
    fn test_should_treat_edits_after_a_conditional_rebind_as_unknown_in_place() {
        let source = "def f(df, c):\n    if c:\n        df = df.copy()\n    df['b'] = 2\n";
        assert_eq!(in_place(source), Some(vec![ColumnEdit::Unknown]));
    }

    fn delegate(name: &str, via_self: bool, conditional: bool) -> ColumnEdit {
        ColumnEdit::Delegate {
            callee: FnRef {
                via_self,
                module: None,
                name: name.to_string(),
            },
            conditional,
        }
    }

    fn summary(in_place: Vec<ColumnEdit>) -> HelperSummary {
        HelperSummary {
            returned: None,
            in_place,
        }
    }

    #[test]
    fn test_should_resolve_delegates_through_bare_and_self_callees() {
        let class = Some("C".to_string());
        let mut summaries = HashMap::from([
            (
                (None, "leaf".to_string()),
                summary(vec![ColumnEdit::Add(strings(&["z"]))]),
            ),
            (
                (class.clone(), "leaf".to_string()),
                summary(vec![ColumnEdit::Add(strings(&["m"]))]),
            ),
            (
                (class.clone(), "top".to_string()),
                summary(vec![
                    delegate("leaf", true, false),
                    delegate("leaf", false, false),
                ]),
            ),
        ]);

        resolve_delegates(&mut summaries, &HashMap::new(), false);

        assert_eq!(
            summaries[&(class, "top".to_string())].in_place,
            vec![
                ColumnEdit::Add(strings(&["m"])),
                ColumnEdit::Add(strings(&["z"]))
            ]
        );
    }

    #[test]
    fn test_should_drop_delegates_to_unknown_callees_and_self_calls_outside_a_class() {
        let mut summaries = HashMap::from([
            (
                (None, "f".to_string()),
                summary(vec![
                    delegate("missing", false, false),
                    delegate("m", true, false),
                    ColumnEdit::Add(strings(&["a"])),
                ]),
            ),
            (
                (None, "only_calls".to_string()),
                summary(vec![delegate("missing", false, false)]),
            ),
        ]);

        resolve_delegates(&mut summaries, &HashMap::new(), false);

        assert_eq!(
            summaries[&(None, "f".to_string())].in_place,
            vec![ColumnEdit::Add(strings(&["a"]))]
        );
        assert!(!summaries.contains_key(&(None, "only_calls".to_string())));
    }

    #[test]
    fn test_should_make_a_conditional_or_cyclic_delegation_unknown() {
        let mut summaries = HashMap::from([
            (
                (None, "leaf".to_string()),
                summary(vec![ColumnEdit::Add(strings(&["z"]))]),
            ),
            (
                (None, "cond".to_string()),
                summary(vec![delegate("leaf", false, true)]),
            ),
            (
                (None, "a".to_string()),
                summary(vec![delegate("b", false, false)]),
            ),
            (
                (None, "b".to_string()),
                summary(vec![delegate("a", false, false)]),
            ),
        ]);

        resolve_delegates(&mut summaries, &HashMap::new(), false);

        assert_eq!(
            summaries[&(None, "cond".to_string())].in_place,
            vec![ColumnEdit::Unknown]
        );
        assert_eq!(
            summaries[&(None, "a".to_string())].in_place,
            vec![ColumnEdit::Unknown]
        );
    }

    #[test]
    fn test_should_summarize_a_helper_that_only_edits_in_place() {
        let source = "def f(df):\n    df['a'] = 1\n";
        let module = parse_module(source).unwrap().into_syntax();
        let Stmt::FunctionDef(func_def) = &module.body[0] else {
            panic!("expected a function");
        };
        assert_eq!(
            summarize_helper(func_def),
            Some(HelperSummary {
                returned: None,
                in_place: vec![ColumnEdit::Add(strings(&["a"]))],
            })
        );
    }

    #[test]
    fn test_should_collect_the_column_edits_a_helper_makes() {
        let source = "def f(df):\n    df['a'] = 1\n    df[['b', 'c']] = 2\n    df.loc[m, 'd'] = 3\n    del df['a']\n    df.pop('b')\n    df.insert(0, 'e', 1)\n    df.drop(columns=['c'], inplace=True)\n    df.rename(columns={'d': 'x'}, inplace=True)\n    df.columns = ['p', 'q']\n    df.dropna(inplace=True)\n    df.drop([0], inplace=True)\n    return df\n";
        assert_eq!(
            edits(source),
            Some(vec![
                ColumnEdit::Add(strings(&["a"])),
                ColumnEdit::Add(strings(&["b", "c"])),
                ColumnEdit::Add(strings(&["d"])),
                ColumnEdit::Remove(strings(&["a"])),
                ColumnEdit::Remove(strings(&["b"])),
                ColumnEdit::Add(strings(&["e"])),
                ColumnEdit::Remove(strings(&["c"])),
                ColumnEdit::Rename(HashMap::from([("d".to_string(), "x".to_string())])),
                ColumnEdit::Replace(strings(&["p", "q"])),
            ])
        );
    }

    #[test]
    fn test_should_treat_unreadable_or_conditional_edits_as_unknown() {
        for body in [
            "df[name] = 1",
            "del df[name]",
            "df.pop(name)",
            "df.insert(0, name, 1)",
            "df.columns = names",
            "df.set_index('a', inplace=True)",
            "df.drop(columns=names, inplace=True)",
            "df.drop(columns=['a'], inplace=flag)",
            "if flag:\n        df['a'] = 1",
            "for x in xs:\n        df[x] = 1",
        ] {
            let source = format!("def f(df, flag=False):\n    {body}\n    return df\n");
            let found = edits(&source).unwrap_or_else(|| panic!("{body}"));
            assert!(found.contains(&ColumnEdit::Unknown), "{body}: {found:?}");
        }
    }

    #[test]
    fn test_should_ignore_inplace_false_and_edits_on_other_names() {
        let source = "def f(df, other):\n    other['a'] = 1\n    df.drop(columns=['a'], inplace=False)\n    return df\n";
        assert_eq!(edits(source), Some(Vec::new()));
    }

    #[test]
    fn test_should_apply_edits_in_order_and_stop_at_unknown() {
        let base = strings(&["a", "b"]);
        let applied = apply_edits(
            &base,
            &[
                ColumnEdit::Add(strings(&["c", "a"])),
                ColumnEdit::Remove(strings(&["b"])),
                ColumnEdit::Rename(HashMap::from([("c".to_string(), "d".to_string())])),
            ],
        );
        assert_eq!(applied, Some(strings(&["a", "d"])));
        assert_eq!(
            apply_edits(&base, &[ColumnEdit::Replace(strings(&["x"]))]),
            Some(strings(&["x"]))
        );
        assert_eq!(apply_edits(&base, &[ColumnEdit::Unknown]), None);
    }

    #[test]
    fn test_should_ignore_returns_and_bindings_in_nested_scopes() {
        let source = "def clean(df):\n    def inner(df):\n        df = f(df)\n        return 1\n    return df.copy()\n";
        assert!(passthrough(source));
    }

    #[test]
    fn test_should_unwrap_await() {
        assert_eq!(kind("await fetch(df)"), df(Kind::Derived));
    }
}

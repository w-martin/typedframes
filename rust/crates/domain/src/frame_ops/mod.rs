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
use std::ops::ControlFlow;

mod delegates;
mod helper;
mod merge;
mod passthrough;
mod writes;

pub(crate) use delegates::*;
pub(crate) use helper::*;
pub(crate) use merge::*;
use passthrough::*;
pub(crate) use writes::*;

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

#[cfg(test)]
mod tests;

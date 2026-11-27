//! Downstream column-usage observation for a DataFrame origin the checker recognized
//! but couldn't resolve a schema for (an `UntypedSite`).
//!
//! Records which columns the rest of a scope reads off such a frame, as evidence for
//! suggesting a column selection or schema. It makes no claim about correctness: there is
//! no schema here to validate against.
//!
//! A suggestion edits real code, so the scan errs toward recording an `Unknown` other-use:
//! anything it can't attribute to a column, or that lets the frame escape, is one.
//!
//! Scope limits:
//! - A nested function, class, lambda or generator expression body is its own scope and is
//!   not walked. `first_nested_scope_mention` reports whether any of them mention the frame,
//!   since a closure can read columns this scan never sees. A list, set or dict comprehension
//!   runs where it is written, so its body is walked as part of the scope.
//! - A `.method()`/`.attribute` use is tagged with its [`Consumption`], but which names
//!   read every column or none is decided by the table in `column_consuming_methods`.

use crate::column_consuming_methods::classify_name;
use crate::contract::expr_forwards_tainted;
use ruff_python_ast::visitor::{self as ast_visitor, Visitor};
use ruff_python_ast::{self as ast, Expr, Stmt};
use ruff_text_size::{Ranged, TextSize};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

mod expressions;
mod nested;
mod record;
mod row_methods;
mod scan;
mod statements;

pub(crate) use nested::*;
pub(crate) use row_methods::*;
use scan::*;

/// One column subscripted directly off a tracked origin, or a variable derived from
/// it via straight reassignment/forwarding (same taint-growth rule
/// `contract::expr_forwards_tainted` uses for a function's own parameter contract).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ColumnAccess {
    pub(crate) column: String,
    /// Reached only under a branch/loop/`with`/`try`, not on every execution path --
    /// still a real access, but the checker cannot promise it always happens.
    pub(crate) conditional: bool,
    /// 1-indexed source line. Not serialized: in a notebook it would be a line of the
    /// concatenated source. Only suggestions use it.
    #[serde(skip)]
    pub(crate) line: usize,
    /// The helper function this read happens inside, when the frame was passed to one.
    #[serde(skip)]
    pub(crate) via: Option<String>,
}

/// What a function does with one of its parameters, when every use of it is a read of a
/// named column. A parameter with any other use has no summary at all: the caller must
/// then treat passing a frame to it as an unknown use.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ParamUsage {
    /// Index among the positional parameters; `usize::MAX` for a keyword-only one.
    pub(crate) position: usize,
    pub(crate) name: String,
    /// Each column read, and whether every such read is conditional.
    pub(crate) accesses: Vec<(String, bool)>,
}

/// Looks up what a called function does with its parameters.
pub(crate) trait HelperResolver {
    /// The summaries of the function `callee` names (one per parameter that has one), or
    /// `None` when the name can't be tied to a single known function.
    fn usage_of(&self, callee: &ast::ExprName) -> Option<Vec<ParamUsage>>;

    /// Whether `callee` is certainly the builtin of that name, not something this file
    /// defines, imports or rebinds.
    fn is_unshadowed_builtin(&self, callee: &ast::ExprName) -> bool;

    /// The summaries of the method a `self.<method>(...)` call reaches, by the position it
    /// is called with (`self` is not counted), or `None` when the name can't be tied to a
    /// single known method.
    fn method_usage_of(&self, _method: &str) -> Option<Vec<ParamUsage>> {
        None
    }

    /// What kind of function `func` is, for the reason a frame passed to it can't be
    /// followed (one of the `CALLEE_*` labels), or `None` when it can't be told.
    fn callee_class(&self, _func: &Expr) -> Option<&'static str> {
        None
    }
}

/// A resolver that knows no function: every call is an unknown use.
#[cfg(test)]
pub(crate) struct NoHelpers;

#[cfg(test)]
impl HelperResolver for NoHelpers {
    fn usage_of(&self, _callee: &ast::ExprName) -> Option<Vec<ParamUsage>> {
        None
    }

    // Tests have no file to check against, so builtins are taken as unshadowed.
    fn is_unshadowed_builtin(&self, _callee: &ast::ExprName) -> bool {
        true
    }
}

/// Whether a `.method()`/`.attribute` use is known to read every column's actual
/// values, known to read none (pure shape/metadata), or isn't classified -- see
/// `column_consuming_methods` for the table this is computed from. A use with no
/// single name (passed to a call, returned, iterated over, an unrecognized
/// subscript) is always `Unknown`: nothing here can say what it does with the
/// frame, so it's treated the same as an unrecognized name -- conservatively, as
/// still needing the columns nothing else accounted for.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Consumption {
    All,
    None,
    Unknown,
}

/// A way the tracked origin (or a derived variable) was used besides a plain
/// `x["col"]`/`x[["a", "b"]]` subscript -- a method/attribute name, or a short label
/// for a shape with no single name.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OtherUse {
    pub(crate) description: String,
    pub(crate) conditional: bool,
    pub(crate) consumes: Consumption,
    /// See `ColumnAccess::line`.
    #[serde(skip)]
    pub(crate) line: usize,
    /// For a frame passed to a call the checker can't follow: what kind of callee it is,
    /// in words that name nothing from the user's code.
    #[serde(skip)]
    pub(crate) callee: Option<&'static str>,
}

pub(crate) const CALLEE_SAME_FILE: &str =
    "a function in this file whose use of the frame can't be followed";

pub(crate) const CALLEE_IMPORTED: &str = "an imported function the checker has no summary of";

pub(crate) const CALLEE_UNBOUND: &str =
    "a name this file never defines or imports (a builtin, or from a star import)";

pub(crate) const CALLEE_MODULE_FUNCTION: &str = "a function of an imported module";

pub(crate) const CALLEE_IMPORTED_NAME: &str =
    "a function or method reached through an imported name";

pub(crate) const CALLEE_OWN_METHOD: &str = "a method of this class that can't be followed";

pub(crate) const CALLEE_OTHER_METHOD: &str = "a method of another object";

pub(crate) const CALLEE_FORWARDS: &str = "a function that returns, stores or forwards the frame";

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ColumnUsage {
    pub(crate) accesses: Vec<ColumnAccess>,
    pub(crate) other_uses: Vec<OtherUse>,
    /// Every name that referred to the origin at any point during the scan (the root
    /// plus each alias it was assigned to). Feeds `first_nested_scope_mention`.
    pub(crate) ever_tainted: HashSet<String>,
    /// A column was assigned into the frame (`df["new"] = ...`) somewhere in the scan.
    pub(crate) wrote_columns: bool,
}

/// Scans `body` -- the statements that can run AFTER an origin, in its own scope -- for downstream
/// usage of `root`. `line_of` maps a source offset to its 1-indexed line.
#[cfg(test)]
pub(crate) fn scan_usage<'s>(
    root: &str,
    body: impl IntoIterator<Item = &'s Stmt>,
    line_of: &dyn Fn(TextSize) -> usize,
) -> ColumnUsage {
    scan_usage_with(root, body, line_of, &NoHelpers)
}

/// [`scan_usage`], following the frame into functions `helpers` can summarize.
pub(crate) fn scan_usage_with<'s>(
    root: &str,
    body: impl IntoIterator<Item = &'s Stmt>,
    line_of: &dyn Fn(TextSize) -> usize,
    helpers: &dyn HelperResolver,
) -> ColumnUsage {
    let mut scan = UsageScan {
        helpers,
        tainted: HashSet::from([root.to_string()]),
        depth: 0,
        written: HashSet::new(),
        gone: HashSet::new(),
        usage: ColumnUsage {
            ever_tainted: HashSet::from([root.to_string()]),
            ..ColumnUsage::default()
        },
        suppressed_self_reference: None,
        line_of,
    };
    for stmt in body {
        scan.visit_stmt(stmt);
    }
    scan.usage
}

#[cfg(test)]
mod tests;

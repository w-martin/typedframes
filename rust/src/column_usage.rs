//! Downstream column-usage observation for a DataFrame origin the checker recognized
//! but couldn't resolve a schema for (an `UntypedSite`).
//!
//! Phase A3 of the guided-suggestion groundwork: before anything can suggest a
//! schema, it needs to know which columns the rest of the file actually reads off
//! such a frame. This module answers that, purely observationally -- it records
//! what it sees and makes no claim about correctness (there is no schema here to
//! validate against in the first place).
//!
//! Two deliberate scope limits, both disclosed rather than silently worked around:
//!
//! - Only an origin sitting at the TOP LEVEL of its own function or module body is
//!   scanned -- one nested inside an `if`/`for`/`while`/`with`/`try` is skipped
//!   entirely (see `Linter::observe_untyped_site_usage`), since "the rest of this
//!   scope" isn't a single contiguous statement slice once control flow is
//!   involved. Most real origins are a plain top-level `df = ...`, so this covers
//!   the common case without the complexity of resuming a walk mid-tree.
//! - A `.method()`/`.attribute` use is tagged with its [`Consumption`] at record
//!   time, but the classification RULES (which names read every column, which read
//!   none) live in `column_consuming_methods`, not here -- this module only ever
//!   calls that table, it doesn't decide what's in it.

use crate::column_consuming_methods::classify_name;
use crate::contract::expr_forwards_tainted;
use ruff_python_ast::{Expr, Stmt};
use serde::Serialize;
use std::collections::HashSet;

/// One column subscripted directly off a tracked origin, or a variable derived from
/// it via straight reassignment/forwarding (same taint-growth rule
/// `contract::expr_forwards_tainted` uses for a function's own parameter contract).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ColumnAccess {
    pub(crate) column: String,
    /// Reached only under a branch/loop/`with`/`try`, not on every execution path --
    /// still a real access, but the checker cannot promise it always happens.
    pub(crate) conditional: bool,
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
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ColumnUsage {
    pub(crate) accesses: Vec<ColumnAccess>,
    pub(crate) other_uses: Vec<OtherUse>,
}

/// Scans `body` -- the statements AFTER an origin, in its own scope -- for downstream
/// usage of `root`.
pub(crate) fn scan_usage(root: &str, body: &[Stmt]) -> ColumnUsage {
    let mut scan = UsageScan {
        tainted: HashSet::from([root.to_string()]),
        depth: 0,
        usage: ColumnUsage::default(),
    };
    for stmt in body {
        scan.visit_stmt(stmt);
    }
    scan.usage
}

struct UsageScan {
    tainted: HashSet<String>,
    // Nesting inside if/for/while/with/try -- marks an access/use `conditional`
    // rather than something every execution path is known to reach.
    depth: u32,
    usage: ColumnUsage,
}

impl UsageScan {
    fn is_tainted(&self, expr: &Expr) -> bool {
        matches!(expr, Expr::Name(n) if self.tainted.contains(n.id.as_str()))
    }

    // A name being (re)assigned either joins the tainted set (its value forwards an
    // already-tainted one) or, if it was PREVIOUSLY tainted, leaves it -- a name
    // reassigned to something unrelated no longer refers to the tracked origin, the
    // same "the old leg ends here" rule `Linter::end_stale_legs` applies elsewhere.
    // Without this, a name reassigned away from the origin would keep contributing
    // observations that aren't actually about it.
    fn retaint(&mut self, name: &str, forwards: bool) {
        if forwards {
            self.tainted.insert(name.to_string());
        } else {
            self.tainted.remove(name);
        }
    }

    fn record_column(&mut self, column: String) {
        self.usage.accesses.push(ColumnAccess {
            column,
            conditional: self.depth > 0,
        });
    }

    fn record_other(&mut self, description: impl Into<String>, consumes: Consumption) {
        self.usage.other_uses.push(OtherUse {
            description: description.into(),
            conditional: self.depth > 0,
            consumes,
        });
    }

    fn visit_body(&mut self, body: &[Stmt]) {
        for stmt in body {
            self.visit_stmt(stmt);
        }
    }

    fn visit_conditional_body(&mut self, body: &[Stmt]) {
        self.depth += 1;
        self.visit_body(body);
        self.depth -= 1;
    }

    fn visit_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            // A nested scope has its own bindings; the taint set built up so far
            // doesn't carry into it (matches `frame_ops::PassthroughScan`'s own
            // treatment of a nested def/class).
            Stmt::FunctionDef(_) | Stmt::ClassDef(_) => {}
            Stmt::Return(ret) => match &ret.value {
                Some(value) if self.is_tainted(value) => {
                    self.record_other("returned", Consumption::Unknown)
                }
                Some(value) => self.visit_expr(value),
                None => {}
            },
            Stmt::Expr(expr_stmt) => self.visit_expr(&expr_stmt.value),
            Stmt::Assign(assign) => {
                self.visit_expr(&assign.value);
                let forwards = expr_forwards_tainted(&self.tainted, &assign.value);
                for target in &assign.targets {
                    if let Expr::Name(t) = target {
                        self.retaint(t.id.as_str(), forwards);
                    }
                }
            }
            Stmt::AnnAssign(ann) => {
                if let Some(value) = &ann.value {
                    self.visit_expr(value);
                    if let Expr::Name(t) = &*ann.target {
                        self.retaint(t.id.as_str(), expr_forwards_tainted(&self.tainted, value));
                    }
                }
            }
            Stmt::AugAssign(aug) => self.visit_expr(&aug.value),
            Stmt::Delete(del) => {
                for target in &del.targets {
                    self.visit_expr(target);
                }
            }
            Stmt::If(if_stmt) => {
                self.visit_expr(&if_stmt.test);
                self.visit_conditional_body(&if_stmt.body);
                for clause in &if_stmt.elif_else_clauses {
                    if let Some(test) = &clause.test {
                        self.visit_expr(test);
                    }
                    self.visit_conditional_body(&clause.body);
                }
            }
            Stmt::For(for_stmt) => {
                if self.is_tainted(&for_stmt.iter) {
                    self.record_other("iterated over directly", Consumption::Unknown);
                } else {
                    self.visit_expr(&for_stmt.iter);
                }
                self.visit_conditional_body(&for_stmt.body);
            }
            Stmt::While(while_stmt) => {
                self.visit_expr(&while_stmt.test);
                self.visit_conditional_body(&while_stmt.body);
            }
            Stmt::With(with_stmt) => {
                for item in &with_stmt.items {
                    self.visit_expr(&item.context_expr);
                }
                self.visit_conditional_body(&with_stmt.body);
            }
            Stmt::Try(try_stmt) => {
                self.visit_conditional_body(&try_stmt.body);
                for handler in &try_stmt.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(h) = handler;
                    self.visit_conditional_body(&h.body);
                }
                self.visit_conditional_body(&try_stmt.orelse);
                self.visit_conditional_body(&try_stmt.finalbody);
            }
            _ => {}
        }
    }

    fn visit_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Subscript(sub) => {
                if self.is_tainted(&sub.value) {
                    match crate::ast_extract::extract_string_list_or_single(&sub.slice) {
                        Some(cols) => {
                            for col in cols {
                                self.record_column(col);
                            }
                        }
                        None => self.record_other("this subscript", Consumption::Unknown),
                    }
                } else {
                    self.visit_expr(&sub.value);
                }
                self.visit_expr(&sub.slice);
            }
            Expr::Attribute(attr) => {
                if self.is_tainted(&attr.value) {
                    let name = attr.attr.as_str();
                    self.record_other(format!("`.{name}`"), classify_name(name));
                } else {
                    self.visit_expr(&attr.value);
                }
            }
            Expr::Call(call) => {
                match &*call.func {
                    // A method call ON the tainted var: `var.method(...)` -- one
                    // "other use" for the whole call, not also the generic
                    // attribute-access case above.
                    Expr::Attribute(attr) if self.is_tainted(&attr.value) => {
                        let name = attr.attr.as_str();
                        self.record_other(format!("`.{name}()`"), classify_name(name));
                    }
                    _ => self.visit_expr(&call.func),
                }
                for arg in &call.arguments.args {
                    if self.is_tainted(arg) {
                        self.record_other("passed to a call", Consumption::Unknown);
                    } else {
                        self.visit_expr(arg);
                    }
                }
                for kw in &call.arguments.keywords {
                    if self.is_tainted(&kw.value) {
                        self.record_other("passed to a call", Consumption::Unknown);
                    } else {
                        self.visit_expr(&kw.value);
                    }
                }
            }
            Expr::BinOp(binop) => {
                self.visit_expr(&binop.left);
                self.visit_expr(&binop.right);
            }
            Expr::BoolOp(boolop) => {
                for v in &boolop.values {
                    self.visit_expr(v);
                }
            }
            Expr::UnaryOp(unary) => self.visit_expr(&unary.operand),
            Expr::Compare(compare) => {
                self.visit_expr(&compare.left);
                for comp in compare.comparators.iter() {
                    self.visit_expr(comp);
                }
            }
            Expr::List(list) => {
                for el in &list.elts {
                    self.visit_expr(el);
                }
            }
            Expr::Tuple(tuple) => {
                for el in &tuple.elts {
                    self.visit_expr(el);
                }
            }
            Expr::Starred(starred) => self.visit_expr(&starred.value),
            Expr::If(if_expr) => {
                self.visit_expr(&if_expr.test);
                self.visit_expr(&if_expr.body);
                self.visit_expr(&if_expr.orelse);
            }
            Expr::Await(awaited) => self.visit_expr(&awaited.value),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruff_python_parser::parse_module;

    fn usage(source: &str) -> ColumnUsage {
        let module = parse_module(source).unwrap().into_syntax();
        scan_usage("df", &module.body)
    }

    fn cols(usage: &ColumnUsage) -> Vec<(&str, bool)> {
        usage
            .accesses
            .iter()
            .map(|a| (a.column.as_str(), a.conditional))
            .collect()
    }

    fn others(usage: &ColumnUsage) -> Vec<(&str, bool, Consumption)> {
        usage
            .other_uses
            .iter()
            .map(|u| (u.description.as_str(), u.conditional, u.consumes))
            .collect()
    }

    #[test]
    fn test_should_record_an_unconditional_column_access() {
        let u = usage("print(df[\"a\"])\nprint(df[\"b\"])\n");
        assert_eq!(cols(&u), vec![("a", false), ("b", false)]);
        assert!(u.other_uses.is_empty());
    }

    #[test]
    fn test_should_record_a_list_subscript_as_multiple_accesses() {
        let u = usage("print(df[[\"a\", \"b\"]])\n");
        assert_eq!(cols(&u), vec![("a", false), ("b", false)]);
    }

    #[test]
    fn test_should_mark_an_access_inside_a_branch_as_conditional() {
        let u = usage("if flag:\n    print(df[\"a\"])\nprint(df[\"b\"])\n");
        assert_eq!(cols(&u), vec![("a", true), ("b", false)]);
    }

    #[test]
    fn test_should_mark_accesses_inside_for_while_with_try_as_conditional() {
        let u = usage(
            "for x in xs:\n    print(df[\"a\"])\nwhile flag:\n    print(df[\"b\"])\nwith open(p) as f:\n    print(df[\"c\"])\ntry:\n    print(df[\"d\"])\nexcept Exception:\n    print(df[\"e\"])\n",
        );
        assert_eq!(
            cols(&u),
            vec![
                ("a", true),
                ("b", true),
                ("c", true),
                ("d", true),
                ("e", true)
            ]
        );
    }

    #[test]
    fn test_should_follow_a_variable_derived_from_the_root_via_reassignment() {
        let u = usage("out = df\nprint(out[\"a\"])\n");
        assert_eq!(cols(&u), vec![("a", false)]);
    }

    #[test]
    fn test_should_follow_a_variable_derived_via_a_call_forwarding_the_root() {
        let u = usage("out = clean(df)\nprint(out[\"a\"])\n");
        assert_eq!(cols(&u), vec![("a", false)]);
    }

    #[test]
    fn test_should_not_follow_an_unrelated_variable() {
        let u = usage("other = something_else()\nprint(other[\"a\"])\n");
        assert!(u.accesses.is_empty());
    }

    #[test]
    fn test_should_record_a_method_call_as_an_other_use_not_an_access() {
        let u = usage("df.to_dict()\n");
        assert_eq!(others(&u), vec![("`.to_dict()`", false, Consumption::All)]);
        assert!(u.accesses.is_empty());
    }

    #[test]
    fn test_should_record_an_attribute_access_as_an_other_use() {
        let u = usage("print(df.columns)\n");
        assert_eq!(others(&u), vec![("`.columns`", false, Consumption::None)]);
    }

    #[test]
    fn test_should_record_being_passed_to_a_call_as_an_other_use() {
        let u = usage("helper(df)\n");
        assert_eq!(
            others(&u),
            vec![("passed to a call", false, Consumption::Unknown)]
        );
    }

    #[test]
    fn test_should_record_being_returned_as_an_other_use() {
        let u = usage("def f():\n    return df\n");
        // `return` inside a nested function isn't walked (own scope) -- construct a
        // bare top-level return instead via a for-loop body to keep it in scope.
        assert!(u.other_uses.is_empty());
        let u2 = usage("if flag:\n    return df\n");
        assert_eq!(others(&u2), vec![("returned", true, Consumption::Unknown)]);
    }

    #[test]
    fn test_should_record_a_direct_iteration_as_an_other_use() {
        // arrange / act: the iterable itself is evaluated once, unconditionally, the
        // moment the `for` statement runs -- the LOOP BODY is what's conditional
        // (depth only increments once entering it, checked separately below).
        let u = usage("for col in df:\n    print(col)\n");

        // assert
        assert_eq!(
            others(&u),
            vec![("iterated over directly", false, Consumption::Unknown)]
        );
    }

    #[test]
    fn test_should_mark_the_for_loop_body_conditional_but_not_its_header() {
        // arrange / act
        let u = usage("for x in xs:\n    print(df[\"a\"])\n");

        // assert
        assert_eq!(cols(&u), vec![("a", true)]);
    }

    #[test]
    fn test_should_record_an_unrecognized_subscript_as_an_other_use() {
        let u = usage("mask = df[\"a\"] > 1\nprint(df[mask])\n");
        // `df["a"]` in the mask expression is a real, unconditional access; the
        // boolean-mask subscript on the next line is an unrecognized shape.
        assert_eq!(cols(&u), vec![("a", false)]);
        assert_eq!(
            others(&u),
            vec![("this subscript", false, Consumption::Unknown)]
        );
    }

    #[test]
    fn test_should_classify_an_unrecognized_method_name_as_unknown() {
        let u = usage("df.some_custom_transform()\n");
        assert_eq!(
            others(&u),
            vec![("`.some_custom_transform()`", false, Consumption::Unknown)]
        );
    }

    #[test]
    fn test_should_classify_a_non_consuming_attribute_by_name() {
        let u = usage("print(df.shape)\n");
        assert_eq!(others(&u), vec![("`.shape`", false, Consumption::None)]);
    }

    #[test]
    fn test_should_not_descend_into_a_nested_function_or_class() {
        let u = usage("def f():\n    print(df[\"a\"])\n\n\nclass C:\n    x = df[\"b\"]\n");
        assert!(u.accesses.is_empty());
        assert!(u.other_uses.is_empty());
    }

    #[test]
    fn test_should_stop_growing_taint_from_an_unrelated_reassignment() {
        // `df = other()` doesn't forward the root, so `df` is no longer tainted from
        // that point -- matches `expr_forwards_tainted`'s own rule.
        let u = usage("df = other()\nprint(df[\"a\"])\n");
        assert!(u.accesses.is_empty());
    }

    #[test]
    fn test_should_untaint_a_name_reassigned_away_from_the_origin() {
        // arrange / act: `out` picks up the taint from `df`, then loses it once
        // reassigned to something unrelated -- the access after that must not be
        // recorded as if it were still about the original origin.
        let u = usage("out = df\nout = other()\nprint(out[\"a\"])\n");

        // assert
        assert!(u.accesses.is_empty());
    }

    #[test]
    fn test_should_keep_the_root_tainted_when_a_derived_alias_is_reassigned() {
        // arrange / act: only `out` loses taint here, not `df` itself.
        let u = usage("out = df\nout = other()\nprint(df[\"a\"])\n");

        // assert
        assert_eq!(cols(&u), vec![("a", false)]);
    }
}

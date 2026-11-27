use super::*;

// AST visitor collecting every bare-Name variable subscripted (`x["col"]`) or having a
// RESERVED_METHODS-named method called on it (`x.groupby(...)`) anywhere in the module
// — see `Linter::dataframe_shaped_usage`. Implements `Visitor` for the same reason as
// `StringBindingCollector`: nested bodies (if/for/while/with/try) are covered by the
// trait's default recursion for free. Flat and file-wide rather than scope-aware,
// matching `string_var_candidates`'s own tradeoff (see its docs) — a name collision
// across scopes can only make this signal slightly noisier, never silently miss a
// genuine one.
pub(super) struct DataFrameShapedUsageCollector {
    pub(super) names: std::collections::HashSet<String>,
}

impl<'a> Visitor<'a> for DataFrameShapedUsageCollector {
    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::Subscript(subscript) => {
                if let Expr::Name(base) = &*subscript.value {
                    self.names.insert(base.id.to_string());
                }
            }
            Expr::Call(call) => {
                self.note_reserved_method_call(call);
            }
            _ => {}
        }
        ast_visitor::walk_expr(self, expr);
    }
}

impl DataFrameShapedUsageCollector {
    fn note_reserved_method_call(&mut self, call: &ruff_python_ast::ExprCall) {
        if let Expr::Attribute(attr) = &*call.func {
            if let Expr::Name(base) = &*attr.value {
                if RESERVED_METHODS.contains(&attr.attr.as_str()) {
                    self.names.insert(base.id.to_string());
                }
            }
        }
    }
}

// Syntactic position a DataFrame-shaped call was found in, tracked by
// `LoadCallSiteCollector` as it descends. Purely descriptive text for
// `--coverage-detail=explain`'s "why wasn't this counted" message -- it never
// affects `dataframes_total`/`dataframes_typed`, which come entirely from the
// targeted Assign/AnnAssign-only counting logic elsewhere in this file. Because of
// that separation, a site labelled `DirectAssignment` here is not a guarantee it WAS
// counted (an unusual annotation on an AnnAssign might still not resolve, e.g.) --
// only that its position matches the one shape the counting logic looks at. Counted
// vs not-counted is decided downstream, by cross-referencing `all_dataframe_calls`
// against `typed_sites`/`untyped_sites` by line.
#[derive(Clone, Copy)]
pub(super) enum CallContext {
    DirectAssignment,
    ReturnValue,
    CallArgument,
    CollectionElement,
    Other,
}

impl CallContext {
    pub(super) fn describe(self) -> &'static str {
        match self {
            CallContext::DirectAssignment => {
                "the direct value of a plain `x = ...` / `x: T = ...` assignment"
            }
            CallContext::ReturnValue => "a `return` value, not assigned to a variable first",
            CallContext::CallArgument => "passed directly as a call argument",
            CallContext::CollectionElement => "an element of a list/tuple/set/dict literal",
            CallContext::Other => {
                "nested inside another expression (an operator, attribute chain, \
                 comprehension, or similar)"
            }
        }
    }
}

// AST visitor collecting every `<LOAD_MODULES>.<LOAD_FUNCTIONS>(...)`-shaped call
// anywhere in the module -- see `Linter::all_dataframe_calls`. Deliberately does NOT
// try to replicate the targeted counting logic's full pattern matching (merge/concat,
// `DataFrame[Schema](...)`, `Schema.from_pandas(df)`, SQL connectors, ...): this is a
// dumb, unconditional, position-independent scan by design, so it can never drift out
// of sync with what the real counting logic recognizes as the project evolves.
// Classifying a found site as COUNTED vs NOT COUNTED happens downstream (cli.py, by
// cross-referencing line numbers against `typed_sites`/`untyped_sites`) -- this
// collector's only job is finding sites and describing WHERE syntactically each one
// was found, via `current_context`.
pub(super) struct LoadCallSiteCollector<'a> {
    pub(super) linter: &'a Linter,
    pub(super) sites: Vec<DataFrameCallSite>,
    pub(super) current_context: CallContext,
}

impl<'a> LoadCallSiteCollector<'a> {
    fn load_call_label(call: &ast::ExprCall) -> Option<String> {
        let Expr::Attribute(attr) = &*call.func else {
            return None;
        };
        let Expr::Name(base) = &*attr.value else {
            return None;
        };
        let module = base.id.as_str();
        let func = attr.attr.as_str();
        if LOAD_MODULES.contains(&module) && LOAD_FUNCTIONS.contains(&func) {
            Some(format!("{module}.{func}"))
        } else {
            None
        }
    }

    // Visit `expr` with `ctx` as the context for THIS node only -- restored
    // afterward, so a call found deeper inside `expr` (e.g. a nested load call in an
    // argument) gets its own, more specific context rather than inheriting `ctx`.
    fn visit_in_context(&mut self, expr: &'a Expr, ctx: CallContext) {
        let previous = self.current_context;
        self.current_context = ctx;
        self.visit_expr(expr);
        self.current_context = previous;
    }
}

impl<'a> Visitor<'a> for LoadCallSiteCollector<'a> {
    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::Call(call) => {
                if let Some(label) = Self::load_call_label(call) {
                    let (line, col) = self.linter.source_location(call.range().start());
                    self.sites.push(DataFrameCallSite {
                        line,
                        col,
                        label,
                        context: self.current_context.describe().to_string(),
                    });
                }
                // Recurse into the call's own pieces with their own, more specific
                // contexts instead of falling through to the generic walk_expr
                // below, which would recurse everything with whatever context this
                // Call node itself inherited -- wrong for the arguments, a
                // different syntactic position than the call as a whole.
                if let Expr::Attribute(attr) = &*call.func {
                    self.visit_in_context(&attr.value, CallContext::Other);
                } else {
                    self.visit_in_context(&call.func, CallContext::Other);
                }
                for arg in &call.arguments.args {
                    self.visit_in_context(arg, CallContext::CallArgument);
                }
                for kw in &call.arguments.keywords {
                    self.visit_in_context(&kw.value, CallContext::CallArgument);
                }
            }
            Expr::List(list) => {
                for el in &list.elts {
                    self.visit_in_context(el, CallContext::CollectionElement);
                }
            }
            Expr::Tuple(tuple) => {
                for el in &tuple.elts {
                    self.visit_in_context(el, CallContext::CollectionElement);
                }
            }
            Expr::Set(set) => {
                for el in &set.elts {
                    self.visit_in_context(el, CallContext::CollectionElement);
                }
            }
            Expr::Dict(dict) => {
                for item in &dict.items {
                    if let Some(key) = &item.key {
                        self.visit_in_context(key, CallContext::Other);
                    }
                    self.visit_in_context(&item.value, CallContext::CollectionElement);
                }
            }
            // Everything else (BinOp, BoolOp, Compare, Subscript, Attribute,
            // comprehensions, lambdas, f-strings, ...): descend via the trait's
            // default recursion, resetting to Other first so a call found several
            // levels down isn't mislabeled with a context that only applied to an
            // ancestor several nodes back.
            _ => {
                self.current_context = CallContext::Other;
                ast_visitor::walk_expr(self, expr);
            }
        }
    }

    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::Return(ret) => {
                if let Some(value) = &ret.value {
                    self.visit_in_context(value, CallContext::ReturnValue);
                }
            }
            Stmt::Assign(assign) if matches!(assign.targets.as_slice(), [Expr::Name(_)]) => {
                self.visit_in_context(&assign.value, CallContext::DirectAssignment);
            }
            Stmt::AnnAssign(ann) if matches!(&*ann.target, Expr::Name(_)) => {
                if let Some(value) = &ann.value {
                    self.visit_in_context(value, CallContext::DirectAssignment);
                }
            }
            _ => {
                self.current_context = CallContext::Other;
                ast_visitor::walk_stmt(self, stmt);
            }
        }
    }
}

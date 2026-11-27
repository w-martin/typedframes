use super::*;

// A candidate `string_var_candidates` entry: either resolved to a stable literal value,
// or excluded because more than one binding (of any kind) touched the name. See
// `StringBindingCollector::record`.
pub(crate) enum StringBinding {
    Literal(String),
    Poisoned,
}

// AST visitor that finds every binding site of every name in a module and resolves
// single-binding, string-literal-valued ones. Implements `Visitor` (rather than
// hand-rolling recursion into every statement/expression variant) so that binding forms
// buried inside nested bodies — `if`/`for`/`while`/`with`/`try`/comprehensions/nested
// functions — are covered by the trait's default `walk_*` recursion instead of needing
// to be threaded through by hand.
pub(super) struct StringBindingCollector<'a> {
    pub(super) linter: &'a Linter,
    pub(super) current_file: &'a Path,
    pub(super) reads_used: u32,
    pub(super) bindings: HashMap<String, StringBinding>,
    // Every binding site of every name, and the bool literal each single-name
    // assignment bound: together they give `bool_var_candidates`.
    pub(super) binding_counts: HashMap<String, u32>,
    pub(super) bool_values: HashMap<String, bool>,
}

impl<'a> StringBindingCollector<'a> {
    // Record a binding of `name`. Any name already present (regardless of what it was
    // previously recorded as) is poisoned by this second binding — reassignment,
    // conditional assignment, and augmented assignment all resolve to "exclude" this
    // way without needing to reason about control flow. `resolved: None` means "this
    // binding exists but isn't a literal we can use" (e.g. a for-loop variable, a
    // function parameter, an import) — poisons on first sight too.
    pub(super) fn record(&mut self, name: &str, resolved: Option<String>) {
        *self.binding_counts.entry(name.to_string()).or_default() += 1;
        if self.bindings.contains_key(name) {
            self.bindings
                .insert(name.to_string(), StringBinding::Poisoned);
            return;
        }
        self.bindings.insert(
            name.to_string(),
            match resolved {
                Some(s) => StringBinding::Literal(s),
                None => StringBinding::Poisoned,
            },
        );
    }

    // Poison every bare `Name` reachable inside an assignment-target expression —
    // handles tuple/list unpacking and starred targets. Attribute (`obj.x = ...`) and
    // subscript (`d["x"] = ...`) targets don't bind a plain name at all, so they're
    // left alone entirely (neither recorded nor poisoned).
    pub(super) fn record_target_names(&mut self, target: &Expr) {
        match target {
            Expr::Name(n) => self.record(n.id.as_str(), None),
            Expr::Tuple(t) => {
                for elt in &t.elts {
                    self.record_target_names(elt);
                }
            }
            Expr::List(l) => {
                for elt in &l.elts {
                    self.record_target_names(elt);
                }
            }
            Expr::Starred(s) => self.record_target_names(&s.value),
            _ => {}
        }
    }
}

impl<'a> Visitor<'a> for StringBindingCollector<'a> {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::Assign(assign) => {
                // Only a single bare-Name target is treated as a literal candidate;
                // multi-target assignment (`a = b = "x"`) and destructuring targets
                // poison every name they touch instead of guessing which one "owns"
                // the literal.
                if let [Expr::Name(n)] = assign.targets.as_slice() {
                    let resolved = self.linter.resolve_literal_rhs(
                        &assign.value,
                        self.current_file,
                        &mut self.reads_used,
                    );
                    if let Expr::BooleanLiteral(b) = &*assign.value {
                        self.bool_values.insert(n.id.to_string(), b.value);
                    }
                    self.record(n.id.as_str(), resolved);
                } else {
                    for target in &assign.targets {
                        self.record_target_names(target);
                    }
                }
            }
            Stmt::AnnAssign(ann) => {
                // A bare `x: str` with no value binds nothing at runtime — not a
                // binding site at all, so neither recorded nor poisoned.
                if let (Expr::Name(n), Some(value)) = (&*ann.target, &ann.value) {
                    let resolved = self.linter.resolve_literal_rhs(
                        value,
                        self.current_file,
                        &mut self.reads_used,
                    );
                    self.record(n.id.as_str(), resolved);
                }
            }
            Stmt::AugAssign(aug) => self.record_target_names(&aug.target),
            Stmt::For(for_stmt) => self.record_target_names(&for_stmt.target),
            Stmt::Global(g) => {
                for name in &g.names {
                    self.record(name.as_str(), None);
                }
            }
            Stmt::Nonlocal(nl) => {
                for name in &nl.names {
                    self.record(name.as_str(), None);
                }
            }
            Stmt::Delete(del) => {
                for target in &del.targets {
                    self.record_target_names(target);
                }
            }
            Stmt::Import(imp) => {
                for alias in &imp.names {
                    let bound = alias.asname.as_ref().unwrap_or(&alias.name);
                    self.record(bound.as_str(), None);
                }
            }
            Stmt::ImportFrom(imp) => {
                for alias in &imp.names {
                    let bound = alias.asname.as_ref().unwrap_or(&alias.name);
                    self.record(bound.as_str(), None);
                }
            }
            _ => {}
        }
        ast_visitor::walk_stmt(self, stmt);
    }

    fn visit_except_handler(&mut self, handler: &'a ast::ExceptHandler) {
        let ast::ExceptHandler::ExceptHandler(h) = handler;
        if let Some(name) = &h.name {
            self.record(name.as_str(), None);
        }
        ast_visitor::walk_except_handler(self, handler);
    }

    fn visit_parameter(&mut self, parameter: &'a ast::Parameter) {
        self.record(parameter.name.as_str(), None);
        ast_visitor::walk_parameter(self, parameter);
    }

    fn visit_with_item(&mut self, item: &'a ast::WithItem) {
        if let Some(vars) = &item.optional_vars {
            self.record_target_names(vars);
        }
        ast_visitor::walk_with_item(self, item);
    }

    fn visit_comprehension(&mut self, comp: &'a ast::Comprehension) {
        self.record_target_names(&comp.target);
        ast_visitor::walk_comprehension(self, comp);
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        // Walrus (`q := "..."`) always poisons rather than being resolved: it's almost
        // always used for control flow (`if (q := f()):`), and a containing expression
        // that runs more than once (a loop, a comprehension) would make "the" value of
        // `q` not actually stable the way a single top-level assignment's is.
        if let Expr::Named(named) = expr {
            if let Expr::Name(n) = &*named.target {
                self.record(n.id.as_str(), None);
            }
        }
        ast_visitor::walk_expr(self, expr);
    }
}

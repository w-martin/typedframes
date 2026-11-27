use super::*;

// One candidate name's state -- see `Linter::dict_list_var_candidates`.
pub(super) enum DictListBinding {
    // `name = []`, then the union of every recognized `.append({...})` call's keys, in
    // first-seen order.
    Building(Vec<String>),
    // `name = {"a": ..., "b": ...}` or `name = [{...}, {...}]`: a literal whose keys are
    // the columns it becomes when passed to `DataFrame(name)`. Kept alongside the
    // evaluator because it needs only the keys to be literal, not the values.
    Rows(Vec<String>),
    Poisoned,
}

// The binding a literal on the right of `name = <literal>` creates, if it carries column
// information. Strict: a dict with a `**spread` or a computed key, a list of anything
// but dict literals, and anything empty carry none.
fn literal_binding(value: &Expr) -> Option<DictListBinding> {
    match value {
        Expr::Dict(dict) => ast_extract::extract_dict_literal_keys(dict).map(DictListBinding::Rows),
        Expr::List(list) => {
            ast_extract::extract_records_list_columns(list).map(DictListBinding::Rows)
        }
        _ => None,
    }
}

// AST visitor backing `Linter::dict_list_var_candidates` -- see that field's doc
// comment for the exact policy. `accounted_for` holds the source range of every Name
// node already explained by a recognized `.append({...})` receiver or `DataFrame(name)`
// argument, so the generic "any other appearance poisons it" rule in `visit_expr`
// doesn't also fire for those two approved occurrences.
pub(super) struct DictListBindingCollector {
    pub(super) bindings: HashMap<String, DictListBinding>,
    pub(super) accounted_for: std::collections::HashSet<ruff_text_size::TextRange>,
}

impl DictListBindingCollector {
    pub(super) fn poison(&mut self, name: &str) {
        self.bindings
            .insert(name.to_string(), DictListBinding::Poisoned);
    }

    // Poison every bare name an assignment/for/with/del target touches -- mirrors
    // `StringBindingCollector::record_target_names`.
    pub(super) fn record_target_names(&mut self, target: &Expr) {
        match target {
            Expr::Name(n) => self.poison(n.id.as_str()),
            Expr::Tuple(t) => t.elts.iter().for_each(|e| self.record_target_names(e)),
            Expr::List(l) => l.elts.iter().for_each(|e| self.record_target_names(e)),
            Expr::Starred(s) => self.record_target_names(&s.value),
            _ => {}
        }
    }

    // `name.append(arg)`: extends the candidate with `arg`'s keys if it's a dict
    // literal of string-literal keys, else poisons it. A no-op for a name that was
    // never initialized as `= []` in the first place.
    fn record_append(&mut self, name: &str, arg: Option<&Expr>) {
        if !self.bindings.contains_key(name) {
            return;
        }
        let extracted = match arg {
            Some(Expr::Dict(dict)) => dict
                .items
                .iter()
                .map(|item| {
                    item.key
                        .as_ref()
                        .and_then(|k| ast_extract::extract_string_literal(k))
                        .map(str::to_string)
                })
                .collect::<Option<Vec<String>>>(),
            _ => None,
        };
        let Some(binding) = self.bindings.get_mut(name) else {
            return;
        };
        match (binding, extracted) {
            (DictListBinding::Poisoned, _) => {}
            // Appending to a literal list changes the columns it stands for.
            (binding @ DictListBinding::Rows(_), _) => *binding = DictListBinding::Poisoned,
            (binding, None) => *binding = DictListBinding::Poisoned,
            (DictListBinding::Building(keys), Some(new_keys)) => {
                for key in new_keys {
                    if !keys.contains(&key) {
                        keys.push(key);
                    }
                }
            }
        }
    }

    fn record_assignment(&mut self, assign: &ruff_python_ast::StmtAssign) {
        if let [Expr::Name(target)] = assign.targets.as_slice() {
            self.record_name_assignment(assign, target);
        } else {
            for target in &assign.targets {
                self.record_target_names(target);
            }
        }
    }

    fn record_name_assignment(
        &mut self,
        assign: &ruff_python_ast::StmtAssign,
        target: &ruff_python_ast::ExprName,
    ) {
        let name = target.id.as_str();
        if matches!(&*assign.value, Expr::List(list) if list.elts.is_empty()) {
            if self.bindings.contains_key(name) {
                self.poison(name);
            } else {
                self.bindings
                    .insert(name.to_string(), DictListBinding::Building(Vec::new()));
                // The defining target itself is a bare Name reference too --
                // exempt it from the generic poison-on-any-appearance rule
                // below, the same way an `.append` receiver or a
                // `DataFrame(name)` argument is.
                self.accounted_for.insert(target.range());
            }
        } else if let Some(binding) = literal_binding(&assign.value) {
            if self.bindings.contains_key(name) {
                self.poison(name);
            } else {
                self.bindings.insert(name.to_string(), binding);
                self.accounted_for.insert(target.range());
            }
        } else {
            self.poison(name);
        }
    }

    fn note_append_call(&mut self, call: &ruff_python_ast::ExprCall) {
        if let Expr::Attribute(attr) = &*call.func {
            if attr.attr.as_str() == "append" {
                if let Expr::Name(recv) = &*attr.value {
                    self.accounted_for.insert(attr.value.range());
                    self.record_append(recv.id.as_str(), call.arguments.args.first());
                }
            }
        }
        if is_dataframe_constructor_callee(&call.func) {
            if let Some(first @ Expr::Name(_)) = call.arguments.args.first() {
                self.accounted_for.insert(first.range());
            }
        }
        // `columns=names` / `usecols=names` on a load or constructor call -- the one
        // place a list of column names is consumed as such.
        if is_load_callee(&call.func) {
            for keyword in call.arguments.keywords.iter() {
                let named = keyword.arg.as_ref().map(|a| a.as_str());
                if matches!(named, Some("columns") | Some("usecols"))
                    && matches!(keyword.value, Expr::Name(_))
                {
                    self.accounted_for.insert(keyword.value.range());
                }
            }
        }
    }
}

impl<'a> Visitor<'a> for DictListBindingCollector {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::Assign(assign) => {
                self.record_assignment(assign);
            }
            Stmt::AnnAssign(ann) => {
                if let Expr::Name(n) = &*ann.target {
                    self.poison(n.id.as_str());
                }
            }
            Stmt::AugAssign(aug) => self.record_target_names(&aug.target),
            Stmt::For(for_stmt) => self.record_target_names(&for_stmt.target),
            Stmt::Global(g) => {
                for name in &g.names {
                    self.poison(name.as_str());
                }
            }
            Stmt::Nonlocal(nl) => {
                for name in &nl.names {
                    self.poison(name.as_str());
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
                    self.poison(bound.as_str());
                }
            }
            Stmt::ImportFrom(imp) => {
                for alias in &imp.names {
                    let bound = alias.asname.as_ref().unwrap_or(&alias.name);
                    self.poison(bound.as_str());
                }
            }
            _ => {}
        }
        ast_visitor::walk_stmt(self, stmt);
    }

    fn visit_except_handler(&mut self, handler: &'a ast::ExceptHandler) {
        let ast::ExceptHandler::ExceptHandler(h) = handler;
        if let Some(name) = &h.name {
            self.poison(name.as_str());
        }
        ast_visitor::walk_except_handler(self, handler);
    }

    fn visit_parameter(&mut self, parameter: &'a ast::Parameter) {
        self.poison(parameter.name.as_str());
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
        if let Expr::Call(call) = expr {
            self.note_append_call(call);
        }
        if let Expr::Named(named) = expr {
            if let Expr::Name(n) = &*named.target {
                self.poison(n.id.as_str());
            }
        }
        if let Expr::Name(name) = expr {
            if !self.accounted_for.contains(&expr.range()) {
                if let Some(DictListBinding::Building(_) | DictListBinding::Rows(_)) =
                    self.bindings.get(name.id.as_str())
                {
                    self.poison(name.id.as_str());
                }
            }
        }
        ast_visitor::walk_expr(self, expr);
    }
}

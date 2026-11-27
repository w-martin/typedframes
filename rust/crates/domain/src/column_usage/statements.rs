use super::*;

impl UsageScan<'_> {
    pub(super) fn visit_stmt(&mut self, stmt: &Stmt) {
        let at = stmt.range().start();
        match stmt {
            // A nested scope has its own bindings; the taint set built up so far
            // doesn't carry into it (matches `frame_ops::PassthroughScan`'s own
            // treatment of a nested def/class). What the statement itself evaluates in
            // THIS scope -- decorators, defaults, class bases -- is still visited, and
            // `first_nested_scope_mention` covers the body.
            Stmt::FunctionDef(f) => {
                for decorator in &f.decorator_list {
                    self.visit_expr(&decorator.expression);
                }
                self.visit_parameter_defaults(&f.parameters);
            }
            Stmt::ClassDef(c) => {
                self.visit_class_def(c);
            }
            Stmt::Return(ret) => match &ret.value {
                Some(value) if self.is_tainted(value) => {
                    self.record_other(at, "returned", Consumption::Unknown)
                }
                Some(value) => self.visit_expr(value),
                None => {}
            },
            Stmt::Expr(expr_stmt) if self.inplace_literal_rename(&expr_stmt.value) => {}
            Stmt::Expr(expr_stmt) => match self.display_only_use(&expr_stmt.value) {
                Some(name) => {
                    self.record_bare_attribute(expr_stmt, name);
                }
                None => self.visit_expr(&expr_stmt.value),
            },
            Stmt::Assign(assign) => {
                // A bare `out = df` is pure aliasing, already fully captured by
                // `rebind_name` below -- must NOT also fall through to `visit_expr`'s
                // generic `Expr::Name` fallback, or a plain rename would wrongly
                // register as an "unrecognized use" alongside becoming tainted. But
                // stored into an attribute or container (`self.df = df`,
                // `cache["k"] = df`) the frame escapes this scan's view entirely.
                self.note_escaping_assignment(at, assign);
            }
            Stmt::AnnAssign(ann) => {
                self.visit_annotated_assign(at, ann);
            }
            // `df["b"] += 1` READS `b` before writing it, so the target is visited as
            // a load -- unlike a plain assignment's.
            Stmt::AugAssign(aug) => {
                self.visit_expr(&aug.value);
                self.visit_expr(&aug.target);
            }
            Stmt::Delete(del) => {
                for target in &del.targets {
                    self.visit_expr(target);
                }
            }
            Stmt::If(if_stmt) => {
                self.visit_if(if_stmt);
            }
            Stmt::For(for_stmt) => {
                self.visit_for(for_stmt);
            }
            Stmt::While(while_stmt) => {
                self.visit_expr(&while_stmt.test);
                self.visit_conditional_body(&while_stmt.body);
                self.visit_conditional_body(&while_stmt.orelse);
            }
            Stmt::With(with_stmt) => {
                self.visit_with(with_stmt);
            }
            Stmt::Try(try_stmt) => {
                self.visit_try(try_stmt);
            }
            Stmt::Assert(assert_stmt) => {
                self.visit_expr(&assert_stmt.test);
                if let Some(msg) = &assert_stmt.msg {
                    self.visit_expr(msg);
                }
            }
            Stmt::Raise(raise_stmt) => {
                if let Some(exc) = &raise_stmt.exc {
                    self.visit_expr(exc);
                }
                if let Some(cause) = &raise_stmt.cause {
                    self.visit_expr(cause);
                }
            }
            Stmt::Match(match_stmt) => {
                self.visit_match(match_stmt);
            }
            _ => {}
        }
    }

    fn record_bare_attribute(&mut self, expr_stmt: &ruff_python_ast::StmtExpr, name: String) {
        let at = expr_stmt.value.range().start();
        self.record_other(
            at,
            format!("{OUTPUT_USE_PREFIX}the bare expression `.{name}`"),
            classify_name(&name),
        );
    }

    fn visit_with(&mut self, with_stmt: &ruff_python_ast::StmtWith) {
        for item in &with_stmt.items {
            self.visit_expr(&item.context_expr);
            if let Some(vars) = &item.optional_vars {
                self.visit_store_target(vars, None);
            }
        }
        self.visit_conditional_body(&with_stmt.body);
    }

    fn visit_if(&mut self, if_stmt: &ruff_python_ast::StmtIf) {
        self.visit_expr(&if_stmt.test);
        self.visit_conditional_body(&if_stmt.body);
        for clause in &if_stmt.elif_else_clauses {
            if let Some(test) = &clause.test {
                self.visit_expr(test);
            }
            self.visit_conditional_body(&clause.body);
        }
    }

    fn visit_match(&mut self, match_stmt: &ruff_python_ast::StmtMatch) {
        self.visit_expr(&match_stmt.subject);
        for case in &match_stmt.cases {
            // A case only runs if its pattern (and guard) matched -- every
            // case is conditional, the same as an `if`/`elif` branch.
            self.depth += 1;
            if let Some(guard) = &case.guard {
                self.visit_expr(guard);
            }
            self.visit_body(&case.body);
            self.depth -= 1;
        }
    }

    fn visit_class_def(&mut self, c: &ruff_python_ast::StmtClassDef) {
        for decorator in &c.decorator_list {
            self.visit_expr(&decorator.expression);
        }
        if let Some(arguments) = &c.arguments {
            for arg in &arguments.args {
                self.visit_expr(arg);
            }
            for keyword in &arguments.keywords {
                self.visit_expr(&keyword.value);
            }
        }
    }

    fn visit_for(&mut self, for_stmt: &ruff_python_ast::StmtFor) {
        if self.is_tainted(&for_stmt.iter) {
            self.record_other(
                for_stmt.iter.range().start(),
                "iterated over directly",
                Consumption::Unknown,
            );
        } else {
            self.visit_expr(&for_stmt.iter);
        }
        self.visit_store_target(&for_stmt.target, None);
        self.visit_conditional_body(&for_stmt.body);
        self.visit_conditional_body(&for_stmt.orelse);
    }

    fn visit_annotated_assign(&mut self, at: TextSize, ann: &ruff_python_ast::StmtAnnAssign) {
        if let Some(value) = &ann.value {
            let escapes = self.is_tainted(value) && !matches!(&*ann.target, Expr::Name(_));
            if escapes {
                self.record_other(
                    at,
                    "stored in an attribute or container",
                    Consumption::Unknown,
                );
            } else if self.derives_same_columns(value) {
                self.record_derivation_effects(value);
            } else {
                self.visit_expr(value);
            }
            let forwards = self.forwards(value);
            self.visit_store_target(&ann.target, Some(forwards));
        }
    }

    fn note_escaping_assignment(&mut self, at: TextSize, assign: &ruff_python_ast::StmtAssign) {
        let escapes = self.is_tainted(&assign.value)
            && assign.targets.iter().any(|t| !matches!(t, Expr::Name(_)));
        if escapes {
            self.record_other(
                at,
                "stored in an attribute or container",
                Consumption::Unknown,
            );
        } else if self.derives_same_columns(&assign.value) {
            self.record_derivation_effects(&assign.value);
        } else {
            self.visit_expr(&assign.value);
        }
        let forwards = self.forwards(&assign.value);
        for target in &assign.targets {
            self.visit_store_target(target, Some(forwards));
        }
    }

    fn visit_try(&mut self, try_stmt: &ruff_python_ast::StmtTry) {
        self.visit_conditional_body(&try_stmt.body);
        for handler in &try_stmt.handlers {
            let ast::ExceptHandler::ExceptHandler(h) = handler;
            if let Some(type_) = &h.type_ {
                self.visit_expr(type_);
            }
            if let Some(name) = &h.name {
                if self.tainted.contains(name.as_str()) {
                    self.record_other(
                        h.range().start(),
                        format!("`{name}` rebound by a loop, `with` or `except` target"),
                        Consumption::Unknown,
                    );
                }
            }
            self.visit_conditional_body(&h.body);
        }
        self.visit_conditional_body(&try_stmt.orelse);
        self.visit_conditional_body(&try_stmt.finalbody);
    }

    // `frame.groupby("key")` / `frame.groupby(["a", "b"], as_index=False)` on the tracked
    // frame: the key columns it reads.
    pub(super) fn groupby_keys(&self, expr: &Expr) -> Option<Vec<String>> {
        let Expr::Call(call) = expr else {
            return None;
        };
        let Expr::Attribute(attr) = &*call.func else {
            return None;
        };
        let receiver = self.tainted_name(&attr.value)?;
        if attr.attr.as_str() != "groupby" || self.is_suppressed(receiver) {
            return None;
        }
        let plain = call.arguments.keywords.iter().all(|k| {
            k.arg.as_ref().is_some_and(|a| {
                matches!(
                    a.as_str(),
                    "as_index" | "sort" | "dropna" | "observed" | "group_keys"
                ) && matches!(&k.value, Expr::BooleanLiteral(_))
            })
        });
        let [keys] = &*call.arguments.args else {
            return None;
        };
        if !plain {
            return None;
        }
        crate::ast_extract::extract_string_list_or_single(keys)
    }

    // `frame.groupby(keys).agg(out=("col", "fn"), ..)` or `.agg({"col": "fn"})`: the keys and
    // the columns aggregated. Any other argument might aggregate every column.
    pub(super) fn record_named_aggregation(&mut self, call: &ast::ExprCall) -> bool {
        let Expr::Attribute(attr) = &*call.func else {
            return false;
        };
        if !matches!(attr.attr.as_str(), "agg" | "aggregate") {
            return false;
        }
        let Some(keys) = self.groupby_keys(&attr.value) else {
            return false;
        };
        let mut sources: Vec<String> = Vec::new();
        for keyword in &call.arguments.keywords {
            let Expr::Tuple(pair) = &keyword.value else {
                return false;
            };
            match pair.elts.first() {
                Some(Expr::StringLiteral(s)) if keyword.arg.is_some() => {
                    sources.push(s.value.to_str().to_string());
                }
                _ => return false,
            }
        }
        match &*call.arguments.args {
            [] => {}
            [Expr::Dict(dict)] if call.arguments.keywords.is_empty() => {
                match crate::ast_extract::extract_string_dict_keys(dict) {
                    Some(names) => sources.extend(names),
                    None => return false,
                }
            }
            _ => return false,
        }
        if sources.is_empty() {
            return false;
        }
        let at = call.range().start();
        for column in keys.into_iter().chain(sources) {
            self.record_column(at, column);
        }
        true
    }
}

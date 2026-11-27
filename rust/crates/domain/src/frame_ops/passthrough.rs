use super::*;

// Whether the parameter still names the caller's own object.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Alias {
    Yes,
    // Rebound under a condition: later edits may or may not reach the caller.
    Maybe,
    No,
}

pub(super) struct PassthroughScan<'p> {
    pub(super) param: &'p str,
    pub(super) saw_return: bool,
    pub(super) ok: bool,
    // Nesting inside if/for/while/try/with/match.
    pub(super) depth: u32,
    pub(super) alias: Alias,
    // Every edit, in order (what the returned frame ends up with).
    pub(super) edits: Vec<ColumnEdit>,
    // The subset that reaches the caller's object.
    pub(super) in_place: Vec<ColumnEdit>,
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
    pub(super) fn rebind(&mut self) {
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

    fn visit_assign(&mut self, assign: &ruff_python_ast::StmtAssign) {
        for target in &assign.targets {
            self.visit_assign_target(assign, target);
        }
    }

    fn visit_assign_target(&mut self, assign: &ruff_python_ast::StmtAssign, target: &Expr) {
        if self.names_param(target) {
            if !(matches!(target, Expr::Name(_)) && preserves_param(&assign.value, self.param)) {
                self.ok = false;
            }
            self.rebind();
        }
        self.record_assignment_edits(target, &assign.value);
    }

    fn visit_helper_assignment(&mut self, assign: &ruff_python_ast::StmtAssign) -> ControlFlow<()> {
        if let Some(value) = self.helper_result_replaces_param(assign) {
            return value;
        }
        ControlFlow::Continue(())
    }

    fn helper_result_replaces_param(
        &mut self,
        assign: &ruff_python_ast::StmtAssign,
    ) -> Option<ControlFlow<()>> {
        if let ([target], Some(callee)) = (assign.targets.as_slice(), self.handed_on(&assign.value))
        {
            if self.is_param(target) {
                self.record_delegate(callee, true);
                if self.alias != Alias::No {
                    self.alias = Alias::Maybe;
                }
                return Some(ControlFlow::Break(()));
            }
        }
        self.visit_assign(assign);
        None
    }

    fn note_return(&mut self, ret: &ruff_python_ast::StmtReturn) {
        self.saw_return = true;
        self.visit_return(ret);
    }

    fn visit_return(&mut self, ret: &ruff_python_ast::StmtReturn) {
        let value = ret.value.as_deref();
        if let Some(callee) = value.and_then(|v| self.handed_on(v)) {
            self.record_delegate(callee, true);
        } else if !value.is_some_and(|value| preserves_param(value, self.param)) {
            self.record_returned_edits(value);
        }
    }

    fn record_returned_edits(&mut self, value: Option<&Expr>) {
        match value.and_then(|value| returned_chain_edits(value, self.param)) {
            Some(edits) => {
                for edit in edits {
                    // On a new object, so the caller's own frame is untouched.
                    self.edits.push(if self.depth > 0 {
                        ColumnEdit::Unknown
                    } else {
                        edit
                    });
                }
            }
            None => self.ok = false,
        }
    }
}

impl<'b> Visitor<'b> for PassthroughScan<'_> {
    fn visit_stmt(&mut self, stmt: &'b Stmt) {
        match stmt {
            // A nested scope has its own returns and its own bindings.
            Stmt::FunctionDef(_) | Stmt::ClassDef(_) => return,
            Stmt::Return(ret) => {
                self.note_return(ret);
            }
            Stmt::Assign(assign) => {
                // `param = g(param)`: the helper's result replaces the parameter. It may
                // be the same object or a copy, so later edits are only possibly in place.
                if let ControlFlow::Break(_) = self.visit_helper_assignment(assign) {
                    return;
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

use super::*;

impl Linter {
    pub(super) fn visit_assign_targets(
        &mut self,
        errors: &mut Vec<LintError>,
        assign: &ruff_python_ast::StmtAssign,
    ) -> (usize, usize) {
        let (current_line, current_col) = self.check_column_writes(errors, assign);

        // A. Multi-column subscript: a = b[["foo", "bar"]]
        self.assign_multi_column_selection(assign, current_line);

        // B. `.loc[:, ["foo", "bar"]]`: the column-list counterpart of A above,
        // for the one other syntax pandas offers for the same "pick these exact
        // columns" read. On any base (tracked or not), same as A -- unlike a
        // WRITE through `.loc[rows, cols] = ...` (see `column_write_target`),
        // this is a read that produces a new, narrower frame. The row part is
        // ignored here (not a column, nothing to infer from); its own column
        // references, if any, are still validated normally by `visit_expr`
        // walking `assign.value` at the end of this arm.
        if let Expr::Subscript(sub) = &*assign.value {
            if let Expr::Attribute(indexer) = &*sub.value {
                if indexer.attr.as_str() == "loc" {
                    self.assign_loc_selection(
                        errors,
                        assign,
                        current_line,
                        current_col,
                        indexer,
                        sub,
                    );
                }
            }
        }
        (current_line, current_col)
    }

    pub(super) fn note_select_statement(&mut self, assign: &ruff_python_ast::StmtAssign) -> usize {
        let dataframes_total_before_call_dispatch = self.dataframes_total;
        // Handle stmt = select(Order.id, Order.amount) — and the same
        // chained onto .where(...)/.order_by(...)/etc, see
        // SELECT_CHAIN_METHODS — so a later pd.read_sql(stmt, engine) can
        // resolve `stmt` via `stmt_var_candidates`. Checked unconditionally
        // here (rather than inside the match below) since the outermost
        // call in the chained form is a `.where(...)` *method* call, not a
        // bare `select(...)` name call, and would otherwise never reach the
        // `Expr::Name(func_name)` arm at all. A later reassignment of the
        // same name simply overwrites this entry (consistent with how
        // `self.variables` already behaves) rather than needing the
        // whole-module poisoning discipline `string_var_candidates` relies
        // on — this only has to be correct in top-to-bottom order, like
        // every other variable binding this checker tracks.
        if let [Expr::Name(target_name)] = assign.targets.as_slice() {
            if let Some(cols) = self.extract_select_columns(&assign.value) {
                self.stmt_var_candidates
                    .insert(target_name.id.to_string(), cols);
            }
        }
        dataframes_total_before_call_dispatch
    }

    pub(super) fn assign_self_method_result(
        &mut self,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        attr: &ruff_python_ast::ExprAttribute,
        func_name: &str,
    ) {
        if let Expr::Attribute(recv_attr) = &*attr.value {
            self.assign_self_attribute_method(
                assign,
                current_line,
                current_col,
                func_name,
                recv_attr,
            );
        }

        // Handle df = self.load() / self.clean(df): a method of the
        // class being visited (or one it inherits), resolved the way a
        // bare function is.
        self.assign_own_method_result(assign, current_line, current_col, attr, func_name);
    }

    fn assign_self_attribute_method(
        &mut self,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        func_name: &str,
        recv_attr: &ruff_python_ast::ExprAttribute,
    ) {
        if let Expr::Name(recv_base) = &*recv_attr.value {
            self.assign_self_attribute_method_of(
                assign,
                current_line,
                current_col,
                func_name,
                recv_attr,
                recv_base,
            );
        }
    }

    fn assign_self_attribute_method_of(
        &mut self,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        func_name: &str,
        recv_attr: &ruff_python_ast::ExprAttribute,
        recv_base: &ruff_python_ast::ExprName,
    ) {
        if recv_base.id.as_str() == "self" {
            self.assign_self_attribute_origin(
                assign,
                current_line,
                current_col,
                func_name,
                recv_attr,
            );
        }
    }

    fn assign_self_attribute_origin(
        &mut self,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        func_name: &str,
        recv_attr: &ruff_python_ast::ExprAttribute,
    ) {
        if let Some(origin) = self
            .current_self_attrs
            .get(recv_attr.attr.as_str())
            .cloned()
        {
            self.assign_origin_method_result(assign, current_line, current_col, func_name, origin);
        }
    }

    fn assign_origin_method_result(
        &mut self,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        func_name: &str,
        origin: SelfAttrOrigin,
    ) {
        if let Some(marker_or_schema) = self
            .class_methods
            .get(&origin.class_name)
            .and_then(|methods| methods.get(func_name))
            .cloned()
        {
            let schema_name =
                self.resolve_open_or_named_schema(&marker_or_schema, func_name, current_line);
            // Confirms any pending pass-through
            // reversal for this method name -- see
            // `pending_passthrough_reversals`.
            self.functions_called.insert(func_name.to_string());
            self.dataframes_total += 1;
            // A bare `-> pd.DataFrame` return (the
            // common shape for a py.typed internal
            // library) lands here as an OPEN schema:
            // typed, but with no column list to check
            // anything against.
            let var_hint = assign_var_hint(assign, func_name);
            self.count_typed_dataframe(&schema_name, &var_hint, current_line, current_col);
            for target in &assign.targets {
                if let Expr::Name(target_name) = target {
                    self.variables.insert(
                        target_name.id.to_string(),
                        (schema_name.clone(), current_line),
                    );
                }
            }
        }
    }

    fn assign_own_method_result(
        &mut self,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        attr: &ruff_python_ast::ExprAttribute,
        func_name: &str,
    ) {
        if let Expr::Name(receiver) = &*attr.value {
            if matches!(receiver.id.as_str(), "self" | "cls") {
                if let Some(marker_or_schema) = self
                    .resolve_self_method(func_name)
                    .filter(|marker| !self.summary_beats_open_marker(marker, &assign.value))
                {
                    self.bind_function_result(
                        assign,
                        &marker_or_schema,
                        func_name,
                        current_line,
                        current_col,
                    );
                } else {
                    let targets = Self::assign_target_names(assign);
                    self.apply_helper_call(&assign.value, &targets, current_line, current_col);
                }
            }
        }
    }

    pub(super) fn validate_pl_col_arguments(
        &mut self,
        errors: &mut Vec<LintError>,
        current_line: usize,
        current_col: usize,
        attr: &ruff_python_ast::ExprAttribute,
        call: &ruff_python_ast::ExprCall,
    ) {
        if let Expr::Name(recv) = &*attr.value {
            self.validate_pl_col_args_on_receiver(
                recv.id.as_str(),
                call,
                current_line,
                current_col,
                errors,
            );
        }
    }
}

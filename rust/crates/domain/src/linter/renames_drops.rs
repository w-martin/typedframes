use super::*;

impl Linter {
    pub(super) fn assign_rename(
        &mut self,
        errors: &mut Vec<LintError>,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        attr: &ruff_python_ast::ExprAttribute,
        call: &ruff_python_ast::ExprCall,
    ) {
        if let Expr::Name(recv) = &*attr.value {
            let recv_str = recv.id.as_str();
            let base_info = self.variables.get(recv_str).map(|(s, l)| (s.clone(), *l));
            let base_schema = base_info.as_ref().map(|(s, _)| s.as_str());
            let base_cols = base_info
                .as_ref()
                .and_then(|(s, _)| self.schemas.get(s).cloned());
            let (mapping, case_fold, target_names) =
                self.rename_of(assign, recv_str, &base_info, &base_cols, call);
            let var_hint = target_names
                .first()
                .cloned()
                .unwrap_or_else(|| recv_str.to_string());
            let base_unresolved = base_info
                .as_ref()
                .is_some_and(|(s, _)| self.schema_is_unresolved(s));
            match (base_cols, mapping, case_fold) {
                (Some(base_cols), None, Some(fold)) => {
                    let new_cols: Vec<String> = base_cols.iter().map(|c| fold.apply(c)).collect();
                    self.register_unresolved_rename(
                        current_line,
                        current_col,
                        base_schema,
                        &target_names,
                        &var_hint,
                        new_cols,
                    );
                }
                (Some(base_cols), Some(mapping), _) => {
                    let schema_display = base_info
                        .as_ref()
                        .map(|(s, l)| self.schema_display(s, *l))
                        .unwrap_or_else(|| "unknown".to_string());
                    // An unresolved base's column list is an
                    // empty placeholder, not a confirmed-empty
                    // schema -- validating old_col names
                    // against it would falsely report every
                    // renamed column as nonexistent.
                    let new_cols = check_renamed_columns_exist(
                        errors,
                        current_line,
                        current_col,
                        base_unresolved,
                        base_cols,
                        mapping,
                        schema_display,
                    );
                    let schema_name = self.count_mutation_leg(
                        base_schema,
                        Some(new_cols),
                        &var_hint,
                        current_line,
                        current_col,
                    );
                    for name in &target_names {
                        self.variables
                            .insert(name.clone(), (schema_name.clone(), current_line));
                    }
                }
                _ if base_info.is_some() => {
                    // A recognized mutator (.rename()) whose
                    // specific edit couldn't be resolved --
                    // e.g. a dynamic mapping -- still counts as
                    // its own origin, just untracked. See
                    // `count_mutation_leg`'s doc comment.
                    let schema_name = self.count_mutation_leg(
                        base_schema,
                        None,
                        &var_hint,
                        current_line,
                        current_col,
                    );
                    for name in &target_names {
                        self.variables
                            .insert(name.clone(), (schema_name.clone(), current_line));
                    }
                }
                _ => {}
            }
        }
    }

    fn register_unresolved_rename(
        &mut self,
        current_line: usize,
        current_col: usize,
        base_schema: Option<&str>,
        target_names: &[String],
        var_hint: &str,
        new_cols: Vec<String>,
    ) {
        let schema_name = self.count_mutation_leg(
            base_schema,
            Some(new_cols),
            var_hint,
            current_line,
            current_col,
        );
        for name in target_names {
            self.variables
                .insert(name.clone(), (schema_name.clone(), current_line));
        }
    }

    pub(super) fn assign_insert(
        &mut self,
        current_line: usize,
        current_col: usize,
        attr: &ruff_python_ast::ExprAttribute,
        call: &ruff_python_ast::ExprCall,
    ) {
        if let Expr::Name(recv) = &*attr.value {
            if let Some(col_name) = call
                .arguments
                .args
                .get(1)
                .and_then(|a| ast_extract::extract_string_literal(a))
            {
                self.add_column_inplace(recv.id.as_str(), col_name, current_line, current_col);
            }
        }
    }

    pub(super) fn visit_expression_statement(
        &mut self,
        errors: &mut Vec<LintError>,
        expr_stmt: &ruff_python_ast::StmtExpr,
    ) {
        let (stmt_line, stmt_col) = self.source_location(expr_stmt.range().start());
        self.apply_helper_call(&expr_stmt.value, &[], stmt_line, stmt_col);
        // Intercept in-place mutations before generic expression visiting.
        if let Expr::Call(call) = &*expr_stmt.value {
            if let Expr::Attribute(attr) = &*call.func {
                let func_name = attr.attr.as_str();
                let (line, col) = self.source_location(call.range().start());
                self.apply_inplace_call(errors, func_name, line, col, attr, call);
            } else if matches!(&*call.func, Expr::Name(_)) {
                // Bare call statement, e.g. `trim_customers(customers)` with no
                // assignment — still validate against the callee's parameter contract.
                let (line, col) = self.source_location(call.range().start());
                self.check_call_requirements(call, line, col, errors);
            }
        }
        self.visit_expr(&expr_stmt.value, errors);
    }

    pub(super) fn assign_drop(
        &mut self,
        errors: &mut Vec<LintError>,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        attr: &ruff_python_ast::ExprAttribute,
        call: &ruff_python_ast::ExprCall,
    ) {
        let Expr::Name(recv) = &*attr.value else {
            return;
        };
        let recv_str = recv.id.as_str();
        let base_info = self.variables.get(recv_str).map(|(s, l)| (s.clone(), *l));
        let base_cols = base_info
            .as_ref()
            .and_then(|(s, _)| self.schemas.get(s).cloned());
        let (dropped, target_names, var_hint, base_unresolved) =
            self.drop_of(assign, attr, recv_str, &base_info, call);
        match (base_cols, dropped) {
            (Some(base_cols), Some(dropped_cols)) => {
                // An unresolved base's column list is an
                // empty placeholder, not a confirmed-empty
                // schema -- validating a drop against it
                // would falsely report every dropped
                // column as nonexistent.
                if !base_unresolved {
                    errors.extend(self.unknown_dropped_columns(
                        (current_line, current_col),
                        &dropped_cols,
                        &base_cols,
                        &base_info,
                    ));
                }
                let new_cols: Vec<String> = base_cols
                    .into_iter()
                    .filter(|c| !dropped_cols.contains(c))
                    .collect();
                let base_schema = base_info.as_ref().map(|(s, _)| s.as_str());
                let schema_name = self.count_mutation_leg(
                    base_schema,
                    Some(new_cols),
                    &var_hint,
                    current_line,
                    current_col,
                );
                for name in &target_names {
                    self.variables
                        .insert(name.clone(), (schema_name.clone(), current_line));
                }
            }
            (base_cols, _) if base_info.is_some() => {
                // A recognized mutator (.drop()) whose
                // specific edit couldn't be resolved --
                // e.g. a dynamic column list -- still
                // counts as its own origin (a known
                // schema-changing method), just untracked
                // rather than silently kept as the base's
                // now possibly-wrong schema.
                self.register_unresolved_drop(
                    current_line,
                    current_col,
                    base_info,
                    target_names,
                    var_hint,
                    base_cols,
                );
            }
            _ => {}
        }
    }

    // Warnings for the dropped columns the base frame doesn't have.
    fn unknown_dropped_columns(
        &self,
        (line, col_at): (usize, usize),
        dropped_cols: &[String],
        base_cols: &[String],
        base_info: &Option<(String, usize)>,
    ) -> Vec<LintError> {
        dropped_cols
            .iter()
            .filter(|col| !base_cols.contains(col))
            .map(|col| {
                let schema_display = base_info
                    .as_ref()
                    .map(|(s, l)| self.schema_display(s, *l))
                    .unwrap_or_else(|| "unknown".to_string());
                LintError {
                    line,
                    col: col_at,
                    code: CODE_DROPPED_UNKNOWN_COLUMN.to_string(),
                    message: format!(
                        "Dropped column '{}' does not exist in {}",
                        col, schema_display
                    ),
                    severity: "warning".to_string(),
                }
            })
            .collect()
    }

    fn register_unresolved_drop(
        &mut self,
        current_line: usize,
        current_col: usize,
        base_info: Option<(String, usize)>,
        target_names: Vec<String>,
        var_hint: String,
        base_cols: Option<Vec<String>>,
    ) {
        let _ = base_cols;
        let base_schema = base_info.as_ref().map(|(s, _)| s.as_str());
        let schema_name =
            self.count_mutation_leg(base_schema, None, &var_hint, current_line, current_col);
        for name in &target_names {
            self.variables
                .insert(name.clone(), (schema_name.clone(), current_line));
        }
    }

    fn drop_of(
        &mut self,
        assign: &ruff_python_ast::StmtAssign,
        attr: &ruff_python_ast::ExprAttribute,
        recv_str: &str,
        base_info: &Option<(String, usize)>,
        call: &ruff_python_ast::ExprCall,
    ) -> (Option<Vec<String>>, Vec<String>, String, bool) {
        let dropped = ast_extract::extract_drop_columns(call, self.is_polars_receiver(&attr.value));
        let target_names: Vec<String> = assign
            .targets
            .iter()
            .filter_map(|t| {
                if let Expr::Name(n) = t {
                    Some(n.id.to_string())
                } else {
                    None
                }
            })
            .collect();
        let var_hint = target_names
            .first()
            .cloned()
            .unwrap_or_else(|| recv_str.to_string());
        let base_unresolved = base_info
            .as_ref()
            .is_some_and(|(s, _)| self.schema_is_unresolved(s));
        (dropped, target_names, var_hint, base_unresolved)
    }

    fn rename_of(
        &mut self,
        assign: &ruff_python_ast::StmtAssign,
        recv_str: &str,
        base_info: &Option<(String, usize)>,
        base_cols: &Option<Vec<String>>,
        call: &ruff_python_ast::ExprCall,
    ) -> (
        Option<HashMap<String, String>>,
        Option<CaseFold>,
        Vec<String>,
    ) {
        let mapping = ast_extract::extract_rename_mapping(call).or_else(|| {
            let cols = base_cols.as_ref()?;
            if base_info
                .as_ref()
                .is_some_and(|(s, _)| self.schema_is_unresolved(s))
            {
                return None;
            }
            Self::evaluated_rename_mapping(call, recv_str, cols)
        });
        let case_fold = if mapping.is_none() {
            ast_extract::extract_rename_case_fold(call)
        } else {
            None
        };
        let target_names: Vec<String> = assign
            .targets
            .iter()
            .filter_map(|t| {
                if let Expr::Name(n) = t {
                    Some(n.id.to_string())
                } else {
                    None
                }
            })
            .collect();
        (mapping, case_fold, target_names)
    }
}

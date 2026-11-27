use super::*;

impl Linter {
    pub(super) fn assign_select(
        &mut self,
        errors: &mut Vec<LintError>,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        attr: &ruff_python_ast::ExprAttribute,
        call: &ruff_python_ast::ExprCall,
    ) {
        if let Expr::Name(recv) = &*attr.value {
            self.assign_select_from(errors, assign, current_line, current_col, call, recv);
        }
    }

    fn assign_select_from(
        &mut self,
        errors: &mut Vec<LintError>,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        call: &ruff_python_ast::ExprCall,
        recv: &ruff_python_ast::ExprName,
    ) {
        let recv_str = recv.id.as_str();
        let base_info = self.variables.get(recv_str).map(|(s, l)| (s.clone(), *l));
        let (base_cols, selected_cols, pl_col_names) = self.selection_of(&base_info, call);
        match selected_cols {
            Some(cols) => {
                if let Some(bc) = &base_cols {
                    errors.extend(self.unknown_selected_columns(
                        (current_line, current_col),
                        &cols,
                        bc,
                        &base_info,
                        &pl_col_names,
                    ));
                }
                self.register_selected_columns(assign, current_line, cols);
            }
            None => {
                // A `.select(...)` whose specific columns
                // couldn't be resolved (`pl.all()`,
                // `pl.exclude(...)`, a dynamic expression,
                // ...) is still a known column-selecting
                // op -- its result must not be assumed to
                // keep the base's full column set, the way
                // an actually row-only op does. Mirrors how
                // `.drop()`/`.rename()` handle an
                // unresolvable specific edit elsewhere in
                // this same dispatch.
                self.register_unresolved_selection(
                    assign,
                    current_line,
                    current_col,
                    recv_str,
                    base_info,
                );
            }
        }
    }

    // Errors for the selected columns the base frame doesn't have.
    fn unknown_selected_columns(
        &self,
        (line, col_at): (usize, usize),
        cols: &[String],
        base_cols: &[String],
        base_info: &Option<(String, usize)>,
        pl_col_names: &std::collections::HashSet<String>,
    ) -> Vec<LintError> {
        cols.iter()
            .filter(|col| {
                pl_col_selection(pl_col_names, col).is_continue() && !base_cols.contains(col)
            })
            .map(|col| {
                let schema_display = base_info
                    .as_ref()
                    .map(|(s, l)| self.schema_display(s, *l))
                    .unwrap_or_else(|| "unknown".to_string());
                LintError {
                    line,
                    col: col_at,
                    code: CODE_UNKNOWN_COLUMN.to_string(),
                    message: format!("Column '{}' does not exist in {}", col, schema_display),
                    severity: "error".to_string(),
                }
            })
            .collect()
    }

    fn register_unresolved_selection(
        &mut self,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        recv_str: &str,
        base_info: Option<(String, usize)>,
    ) {
        if let Some((base_schema, _)) = base_info {
            let var_name = assign_var_hint(assign, recv_str);
            let schema_name = self.count_mutation_leg(
                Some(&base_schema),
                None,
                &var_name,
                current_line,
                current_col,
            );
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

    fn selection_of(
        &mut self,
        base_info: &Option<(String, usize)>,
        call: &ruff_python_ast::ExprCall,
    ) -> (
        Option<Vec<String>>,
        Option<Vec<String>>,
        std::collections::HashSet<String>,
    ) {
        let base_cols = base_info
            .as_ref()
            .filter(|(s, _)| !self.schema_is_unresolved(s))
            .and_then(|(s, _)| self.schemas.get(s).cloned());
        let selected_cols = ast_extract::extract_select_args_columns(call);
        // A name from a `pl.col(...)`/`col(...)` argument is
        // already validated, unconditionally, by
        // `validate_pl_col_args_on_receiver` below -- skip
        // just those names here so they aren't reported
        // twice. A plain string-literal argument mixed into
        // the same call (`select("a", pl.col("b"))`) is NOT
        // covered by that other check (it only ever looks at
        // `pl.col()` names), so it must still be validated
        // here -- skipping the whole call whenever ANY
        // `pl.col()` arg was present used to let a bad
        // literal column through unvalidated.
        let pl_col_names: std::collections::HashSet<String> = call
            .arguments
            .args
            .iter()
            .filter_map(ast_extract::extract_pl_col_name)
            .collect();
        (base_cols, selected_cols, pl_col_names)
    }

    fn register_selected_columns(
        &mut self,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        cols: Vec<String>,
    ) {
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
        let var_name = target_names
            .first()
            .map(|s| s.as_str())
            .unwrap_or("unknown");
        let schema_name = self.make_inferred_schema(cols, var_name, current_line);
        for name in &target_names {
            self.variables
                .insert(name.clone(), (schema_name.clone(), current_line));
        }
    }

    pub(super) fn assign_row_preserving(
        &mut self,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        attr: &ruff_python_ast::ExprAttribute,
        func_name: &str,
    ) {
        if let Expr::Name(recv) = &*attr.value {
            self.assign_row_preserving_from(assign, current_line, current_col, func_name, recv);
        }
    }

    fn assign_row_preserving_from(
        &mut self,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        func_name: &str,
        recv: &ruff_python_ast::ExprName,
    ) {
        if let Some((base_schema, _)) = self.variables.get(recv.id.as_str()) {
            let mut base_schema = base_schema.clone();
            // `reset_index()` of a frame with a known named index
            // brings the levels back as columns.
            if func_name == "reset_index" && self.schema_index.contains_key(&base_schema) {
                if let Some(cols) = self.returned_expr_columns(&assign.value) {
                    base_schema = self.count_mutation_leg(
                        Some(&base_schema),
                        Some(cols),
                        recv.id.as_str(),
                        current_line,
                        current_col,
                    );
                }
            }
            for target in &assign.targets {
                if let Expr::Name(target_name) = target {
                    self.variables.insert(
                        target_name.id.to_string(),
                        (base_schema.clone(), current_line),
                    );
                }
            }
        }
    }

    pub(super) fn assign_column_filter(
        &mut self,
        errors: &mut Vec<LintError>,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        recv_str: &str,
        call: &ruff_python_ast::ExprCall,
    ) {
        let base_info = self.variables.get(recv_str).map(|(s, l)| (s.clone(), *l));
        match ast_extract::extract_filter_items_columns(call) {
            Some(cols) => {
                self.assign_filtered_known_columns(
                    errors,
                    current_line,
                    current_col,
                    &base_info,
                    &cols,
                );
                let var_name = assign_var_hint(assign, recv_str);
                let schema_name = self.make_inferred_schema(cols, &var_name, current_line);
                for target in &assign.targets {
                    if let Expr::Name(target_name) = target {
                        self.variables.insert(
                            target_name.id.to_string(),
                            (schema_name.clone(), current_line),
                        );
                    }
                }
            }
            None => {
                // `like=`/`regex=`, or a non-literal
                // `items=` -- still known to select
                // columns, just not which ones, so this
                // becomes unresolved rather than silently
                // keeping the base's full column set.
                self.assign_filtered_unknown_columns(
                    assign,
                    current_line,
                    current_col,
                    recv_str,
                    base_info,
                );
            }
        }
    }

    fn assign_filtered_known_columns(
        &mut self,
        errors: &mut Vec<LintError>,
        current_line: usize,
        current_col: usize,
        base_info: &Option<(String, usize)>,
        cols: &Vec<String>,
    ) {
        if let Some((base_schema, base_def_line)) = base_info {
            let base_cols = self.schemas.get(base_schema).cloned().unwrap_or_default();
            for col in cols {
                if !base_cols.contains(col) {
                    let schema_display = self.schema_display(base_schema, *base_def_line);
                    errors.push(LintError {
                        line: current_line,
                        col: current_col,
                        code: CODE_UNKNOWN_COLUMN.to_string(),
                        message: format!("Column '{}' does not exist in {}", col, schema_display),
                        severity: "error".to_string(),
                    });
                }
            }
        }
    }

    fn assign_filtered_unknown_columns(
        &mut self,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        recv_str: &str,
        base_info: Option<(String, usize)>,
    ) {
        if let Some((base_schema, _)) = base_info {
            let var_name = assign_var_hint(assign, recv_str);
            let schema_name = self.count_mutation_leg(
                Some(&base_schema),
                None,
                &var_name,
                current_line,
                current_col,
            );
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
}

fn pl_col_selection(
    pl_col_names: &std::collections::HashSet<String>,
    col: &String,
) -> ControlFlow<()> {
    if pl_col_names.contains(col) {
        return ControlFlow::Break(());
    }
    ControlFlow::Continue(())
}

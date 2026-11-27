use super::*;

impl Linter {
    pub(super) fn check_column_writes(
        &mut self,
        errors: &mut Vec<LintError>,
        assign: &ruff_python_ast::StmtAssign,
    ) -> (usize, usize) {
        let (current_line, current_col) = self.source_location(assign.range().start());

        // Check for mutations: df["new_col"] = ..., df[["a", "b"]] = ...,
        // df.loc[rows, "new_col"] = ...
        for target in &assign.targets {
            if let Some((recv, cols)) = column_write_target(target) {
                self.record_column_writes(&recv, &cols, current_line, current_col, errors);
            } else if let Some(recv) = row_insert_receiver(target) {
                // `frame.loc[0] = {..}` on a frame with no columns yet makes them.
                let empty = self
                    .variables
                    .get(&recv)
                    .and_then(|(schema, _)| self.schemas.get(schema))
                    .is_some_and(|columns| columns.is_empty());
                if empty {
                    self.record_dynamic_column_write(&recv, current_line, current_col);
                }
            } else if let Some((recv, key)) = dynamic_column_write_receiver(target) {
                match self.loop_computed_names(key) {
                    Some(cols) => {
                        self.record_column_writes(&recv, &cols, current_line, current_col, errors)
                    }
                    None => self.record_dynamic_column_write(&recv, current_line, current_col),
                }
            }
        }

        // df.columns = df.columns.str.lower() / .str.upper() -- a recognized
        // case-fold of an already-known column set (see CaseFold's doc comment
        // for why this is a fixed, narrow pattern rather than general support
        // for arbitrary transform functions).
        for target in &assign.targets {
            if let ControlFlow::Break(_) =
                self.check_columns_assignment(assign, current_line, target)
            {
                continue;
            }
        }
        (current_line, current_col)
    }

    fn check_columns_assignment(
        &mut self,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        target: &Expr,
    ) -> ControlFlow<()> {
        if let Expr::Attribute(target_attr) = target {
            if target_attr.attr.as_str() != "columns" {
                return ControlFlow::Break(());
            }
            let Expr::Name(target_recv) = &*target_attr.value else {
                return ControlFlow::Break(());
            };
            let Some((rhs_recv, fold)) = ast_extract::extract_columns_str_fold(&assign.value)
            else {
                return ControlFlow::Break(());
            };
            if rhs_recv != target_recv.id.as_str() {
                return ControlFlow::Break(());
            }
            if let Some((schema_name, _)) = self.variables.get(target_recv.id.as_str()).cloned() {
                if let Some(base_cols) = self.schemas.get(&schema_name).cloned() {
                    let new_cols: Vec<String> = base_cols.iter().map(|c| fold.apply(c)).collect();
                    let new_schema =
                        self.make_inferred_schema(new_cols, target_recv.id.as_str(), current_line);
                    self.variables
                        .insert(target_recv.id.to_string(), (new_schema, current_line));
                }
            }
        }
        ControlFlow::Continue(())
    }

    pub(super) fn assign_loc_selection(
        &mut self,
        errors: &mut Vec<LintError>,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        indexer: &ruff_python_ast::ExprAttribute,
        sub: &ruff_python_ast::ExprSubscript,
    ) {
        if let Expr::Name(base_name) = &*indexer.value {
            self.assign_loc_selection_of(errors, assign, current_line, current_col, sub, base_name);
        }
    }

    fn assign_loc_selection_of(
        &mut self,
        errors: &mut Vec<LintError>,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        sub: &ruff_python_ast::ExprSubscript,
        base_name: &ruff_python_ast::ExprName,
    ) {
        let base_str = base_name.id.as_str();
        if let Expr::Tuple(indexers) = &*sub.slice {
            self.assign_loc_rows_and_columns(
                errors,
                assign,
                current_line,
                current_col,
                base_str,
                indexers,
            );
        }
    }

    fn assign_loc_rows_and_columns(
        &mut self,
        errors: &mut Vec<LintError>,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        base_str: &str,
        indexers: &ruff_python_ast::ExprTuple,
    ) {
        if indexers.elts.len() == 2 {
            self.assign_loc_column_list(
                errors,
                assign,
                current_line,
                current_col,
                base_str,
                indexers,
            );
        }
    }

    fn assign_loc_column_list(
        &mut self,
        errors: &mut Vec<LintError>,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        base_str: &str,
        indexers: &ruff_python_ast::ExprTuple,
    ) {
        if let Some(cols) = ast_extract::extract_string_list(&indexers.elts[1]) {
            self.assign_loc_selected_columns(
                errors,
                assign,
                current_line,
                current_col,
                base_str,
                cols,
            );
        }
    }

    fn assign_loc_selected_columns(
        &mut self,
        errors: &mut Vec<LintError>,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        base_str: &str,
        cols: Vec<String>,
    ) {
        let base_info = self.variables.get(base_str).map(|(s, l)| (s.clone(), *l));
        if let Some((base_schema, base_def_line)) = &base_info {
            self.assign_loc_known_base(
                errors,
                current_line,
                current_col,
                &cols,
                base_schema,
                base_def_line,
            );
        }
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
        let var_name = target_names.first().map(|s| s.as_str()).unwrap_or(base_str);
        let schema_name = self.make_inferred_schema(cols, var_name, current_line);
        for name in &target_names {
            self.variables
                .insert(name.clone(), (schema_name.clone(), current_line));
        }
    }

    fn assign_loc_known_base(
        &mut self,
        errors: &mut Vec<LintError>,
        current_line: usize,
        current_col: usize,
        cols: &Vec<String>,
        base_schema: &String,
        base_def_line: &usize,
    ) {
        let base_cols = self.schemas.get(base_schema).cloned().unwrap_or_default();
        if !base_cols.is_empty() {
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

    pub(super) fn apply_inplace_call(
        &mut self,
        errors: &mut Vec<LintError>,
        func_name: &str,
        line: usize,
        col: usize,
        attr: &ruff_python_ast::ExprAttribute,
        call: &ruff_python_ast::ExprCall,
    ) {
        self.apply_inplace_method(attr, call, line, col, errors);
        if func_name == "pop" {
            if let Expr::Name(recv) = &*attr.value {
                if let Some(col_name) = call
                    .arguments
                    .args
                    .first()
                    .and_then(|a| ast_extract::extract_string_literal(a))
                {
                    self.remove_column_inplace(
                        recv.id.as_str(),
                        col_name,
                        line,
                        col,
                        "pop",
                        errors,
                    );
                }
            }
        } else if func_name == "insert" {
            if let Expr::Name(recv) = &*attr.value {
                if let Some(col_name) = call
                    .arguments
                    .args
                    .get(1)
                    .and_then(|a| ast_extract::extract_string_literal(a))
                {
                    self.add_column_inplace(recv.id.as_str(), col_name, line, col);
                }
            }
        } else if func_name == "execute" {
            // cursor.execute(sql) — the PEP 249 pattern used by
            // Snowflake, Redshift, and similar DB-API connectors. The
            // SQL text is bound to the cursor, not returned, so it's
            // tracked in `cursor_sql` until a later
            // `cursor.fetch_pandas_all()` (see the Assign arm) turns it
            // into a DataFrame.
            if let Expr::Name(recv) = &*attr.value {
                let sql = call
                    .arguments
                    .args
                    .first()
                    .and_then(|a| self.extract_sql_expr(a));
                match sql {
                    Some(sql) => {
                        self.cursor_sql.insert(recv.id.to_string(), sql);
                    }
                    None => {
                        self.cursor_sql.remove(recv.id.as_str());
                    }
                }
            }
        }
        // Validate pl.col() / col() references for bare expression method calls.
        if let Expr::Name(recv) = &*attr.value {
            self.validate_pl_col_args_on_receiver(recv.id.as_str(), call, line, col, errors);
            // Bare call statement via a plain `import module`, e.g.
            // `transforms.trim_customers(customers)` — guarded on `recv`
            // not being a tracked DataFrame variable so a genuine method
            // call is never treated as a call to a same-named delegate.
            if !self.variables.contains_key(recv.id.as_str()) {
                self.check_call_requirements(call, line, col, errors);
            }
        }
    }

    pub(super) fn visit_delete(
        &mut self,
        errors: &mut Vec<LintError>,
        delete: &ruff_python_ast::StmtDelete,
    ) {
        for target in &delete.targets {
            self.delete_target_columns(errors, target);
        }
    }

    fn delete_target_columns(&mut self, errors: &mut Vec<LintError>, target: &Expr) {
        if let Expr::Subscript(subscript) = target {
            if let Expr::Name(recv) = &*subscript.value {
                if let Some(col_name) = ast_extract::extract_string_literal(&subscript.slice) {
                    let (line, col) = self.source_location(subscript.range().start());
                    self.remove_column_inplace(
                        recv.id.as_str(),
                        col_name,
                        line,
                        col,
                        "del",
                        errors,
                    );
                }
            }
        }
    }

    pub(super) fn assign_multi_column_selection(
        &mut self,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
    ) {
        if let Expr::Subscript(sub) = &*assign.value {
            self.assign_multi_column_selection_subscript(assign, current_line, sub);
        }
    }

    fn assign_multi_column_selection_subscript(
        &mut self,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        sub: &ruff_python_ast::ExprSubscript,
    ) {
        if let Expr::Name(base_name) = &*sub.value {
            self.assign_multi_column_selection_subscript_name(assign, current_line, sub, base_name);
        }
    }

    fn assign_multi_column_selection_subscript_name(
        &mut self,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        sub: &ruff_python_ast::ExprSubscript,
        base_name: &ruff_python_ast::ExprName,
    ) {
        let base_str = base_name.id.as_str();
        match ast_extract::extract_string_list(&sub.slice) {
            Some(cols) => {
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
            // `df["a"]` is a Series, not a frame: nothing to propagate.
            None if ast_extract::extract_string_literal(&sub.slice).is_some() => {}
            None => {
                // Boolean mask / unknown — passthrough base schema to target
                self.assign_multi_column_boolean_mask_unknown(assign, current_line, base_str);
            }
        }
    }

    fn assign_multi_column_boolean_mask_unknown(
        &mut self,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        base_str: &str,
    ) {
        if let Some((base_schema, _)) = self.variables.get(base_str).map(|(s, l)| (s.clone(), *l)) {
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
            for name in &target_names {
                self.variables
                    .insert(name.clone(), (base_schema.clone(), current_line));
            }
        }
    }
}

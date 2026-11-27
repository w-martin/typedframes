use super::*;

impl Linter {
    pub(super) fn assign_schema_from_frame(
        &mut self,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        class_str: &str,
    ) {
        self.dataframes_total += 1;
        let var_hint = assign_var_hint(assign, class_str);
        self.count_typed_dataframe(class_str, &var_hint, current_line, current_col);
        for target in &assign.targets {
            if let Expr::Name(target_name) = target {
                self.variables.insert(
                    target_name.id.to_string(),
                    (class_str.to_string(), current_line),
                );
            }
        }
    }

    pub(super) fn assign_load(
        &mut self,
        errors: &mut Vec<LintError>,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        func_name: &str,
        call: &ruff_python_ast::ExprCall,
    ) {
        self.dataframes_total += 1;
        let (extracted, load_kind) = self.extract_load_columns(func_name, call);
        match extracted {
            Some(cols) => {
                self.register_load_columns(assign, current_line, current_col, cols);
            }
            None => {
                self.register_unknown_load(assign, current_line, current_col);
                let hint = match load_kind {
                    LoadKind::Sql => {
                        "name the columns in the `SELECT` list \
                                                         instead of `SELECT *` -- an explicit \
                                                         list is what lets this checker (and \
                                                         readers) know which columns actually \
                                                         exist, or annotate the variable's \
                                                         type, e.g. `df: Annotated[pd.DataFrame, \
                                                         MySchema] = pd.read_sql(...)`"
                    }
                    LoadKind::File if has_keyword(call, &["columns", "usecols"]) => {
                        "the column list given isn't made of \
                                                         string literals or constants the \
                                                         checker can resolve (an attribute like \
                                                         `self.x` can't be); use literals or \
                                                         module-level constants, or annotate the \
                                                         variable's type, e.g. `df: \
                                                         Annotated[pd.DataFrame, MySchema] = \
                                                         pd.DataFrame(...)`"
                    }
                    LoadKind::File if func_name == "DataFrame" => {
                        "pass a dict literal, or specify \
                                                         `columns=`, or annotate the variable's \
                                                         type, e.g. `df: Annotated[pd.DataFrame, \
                                                         MySchema] = pd.DataFrame(...)`"
                    }
                    LoadKind::File => {
                        "specify `usecols`/`columns`, or \
                                                         annotate the variable's type, e.g. \
                                                         `df: Annotated[pd.DataFrame, \
                                                         MySchema] = pd.read_csv(...)`"
                    }
                    LoadKind::Orm => {
                        "pass select(Model.col1, Model.col2, ...) \
                                                         referencing a registered model's known \
                                                         columns (a bare `select(Model)` isn't \
                                                         supported), or annotate the variable's \
                                                         type, e.g. `df: Annotated[pd.DataFrame, \
                                                         MySchema] = pd.read_sql(...)`"
                    }
                };
                errors.push(LintError {
                    line: current_line,
                    col: current_col,
                    code: CODE_UNTRACKED_DATAFRAME.to_string(),
                    message: format!("columns unknown at lint time; {hint}"),
                    severity: "warning".to_string(),
                });
            }
        }
    }

    fn register_unknown_load(
        &mut self,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
    ) {
        let var_name = assign
            .targets
            .iter()
            .find_map(|t| {
                if let Expr::Name(n) = t {
                    Some(n.id.to_string())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "df".to_string());
        self.record_untyped_site(&var_name, current_line, current_col);
    }

    pub(super) fn track_assignment_targets(
        &mut self,
        errors: &mut Vec<LintError>,
        assign: &ruff_python_ast::StmtAssign,
    ) {
        for target in &assign.targets {
            // `name["literal"] = ...` was already handled by the mutation-tracking
            // check above; revisiting it here would double-report an unresolved
            // schema (no backstop like the concrete case's `self.schemas` append).
            let already_handled_as_mutation = column_write_target(target)
                .is_some_and(|(recv, _)| self.variables.contains_key(&recv));
            if !already_handled_as_mutation {
                self.visit_expr(target, errors);
            } else if let Some(rows) = loc_row_indexer(target) {
                // The written columns were recorded above; a mask that reads
                // other columns is still an ordinary read.
                self.visit_expr(rows, errors);
            }
        }
        if !self.value_checked_early {
            self.visit_expr(&assign.value, errors);
        }
    }

    pub(super) fn assign_pop(
        &mut self,
        errors: &mut Vec<LintError>,
        current_line: usize,
        current_col: usize,
        attr: &ruff_python_ast::ExprAttribute,
        call: &ruff_python_ast::ExprCall,
    ) {
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
                    current_line,
                    current_col,
                    "pop",
                    errors,
                );
            }
        }
    }

    pub(super) fn assign_connectorx_read(
        &mut self,
        errors: &mut Vec<LintError>,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        call: &ruff_python_ast::ExprCall,
    ) {
        self.dataframes_total += 1;
        let sql = call
            .arguments
            .args
            .get(1)
            .and_then(|a| self.extract_sql_expr(a));
        let cols = sql.and_then(
            |sql| match sql::columns_from_select(&sql, self.sql_dialect) {
                sql::SqlOutcome::Columns(cols) => Some(cols),
                sql::SqlOutcome::Wildcard | sql::SqlOutcome::Unparsed => None,
            },
        );
        self.register_connectorx_frame(errors, assign, current_line, current_col, cols);
    }

    pub(super) fn assign_cursor_fetch(
        &mut self,
        errors: &mut Vec<LintError>,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        attr: &ruff_python_ast::ExprAttribute,
    ) {
        if let Expr::Name(recv) = &*attr.value {
            let sql = self.cursor_sql.get(recv.id.as_str()).cloned();
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
            let var_name = target_names.first().map(|s| s.as_str()).unwrap_or("df");
            self.register_sql_dataframe(
                sql.as_deref(),
                &target_names,
                var_name,
                current_line,
                current_col,
                errors,
            );
        }
    }

    pub(super) fn assign_sql_finalize(
        &mut self,
        errors: &mut Vec<LintError>,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        attr: &ruff_python_ast::ExprAttribute,
    ) {
        if let Some(inner_call) = ast_extract::sql_producing_call(&attr.value) {
            let sql = self.extract_sql_literal(inner_call);
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
            let var_name = target_names.first().map(|s| s.as_str()).unwrap_or("df");
            self.register_sql_dataframe(
                sql.as_deref(),
                &target_names,
                var_name,
                current_line,
                current_col,
                errors,
            );
        }
    }

    fn register_load_columns(
        &mut self,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
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
            .cloned()
            .unwrap_or_else(|| "df".to_string());
        let schema_name = self.make_inferred_schema(cols, &var_name, current_line);
        self.count_typed_dataframe(&schema_name, &var_name, current_line, current_col);
        for name in &target_names {
            self.variables
                .insert(name.clone(), (schema_name.clone(), current_line));
        }
    }

    fn register_connectorx_frame(
        &mut self,
        errors: &mut Vec<LintError>,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        cols: Option<Vec<String>>,
    ) {
        match cols {
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
                    .cloned()
                    .unwrap_or_else(|| "df".to_string());
                let schema_name = self.make_inferred_schema(cols, &var_name, current_line);
                self.count_typed_dataframe(&schema_name, &var_name, current_line, current_col);
                for name in &target_names {
                    self.variables
                        .insert(name.clone(), (schema_name.clone(), current_line));
                }
            }
            None => {
                let var_name = assign
                    .targets
                    .iter()
                    .find_map(|t| {
                        if let Expr::Name(n) = t {
                            Some(n.id.to_string())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| "df".to_string());
                self.record_untyped_site(&var_name, current_line, current_col);
                errors.push(LintError {
                    line: current_line,
                    col: current_col,
                    code: CODE_UNTRACKED_DATAFRAME.to_string(),
                    message: "columns unknown at lint \
                                                              time; name the columns in \
                                                              the `SELECT` list (avoid \
                                                              `SELECT *`) or annotate: \
                                                              `df: Annotated[pd.DataFrame, \
                                                              MySchema] = ...`"
                        .to_string(),
                    severity: "warning".to_string(),
                });
            }
        }
    }
}

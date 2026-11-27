use super::*;

impl Linter {
    pub(super) fn assign_schema_constructor(
        &mut self,
        errors: &mut Vec<LintError>,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        dataframes_total_before_call_dispatch: usize,
        call: &ruff_python_ast::ExprCall,
    ) {
        if let Expr::Subscript(subscript) = &*call.func {
            self.assign_subscripted_constructor(assign, current_line, current_col, subscript);
        } else if let Expr::Attribute(attr) = &*call.func {
            // Handle Schema().read_csv(...) style
            self.assign_schema_method_call(errors, assign, current_line, current_col, call, attr);
        } else if let Expr::Name(func_name) = &*call.func {
            // Handle df = load_users() where load_users() -> DataFrame[Schema]
            // (or a bare -> pd.DataFrame/pl.DataFrame, resolved to an open
            // schema by resolve_open_or_named_schema).
            self.assign_function_result(errors, assign, current_line, current_col, func_name, call);
        }

        self.maybe_record_unresolved_dataframe_shaped_call(
            assign,
            call,
            dataframes_total_before_call_dispatch,
        );
    }

    fn assign_schema_method_call(
        &mut self,
        errors: &mut Vec<LintError>,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        call: &ruff_python_ast::ExprCall,
        attr: &ruff_python_ast::ExprAttribute,
    ) {
        let current_expr = &*attr.value;
        if let Expr::Call(inner_call) = current_expr {
            self.assign_schema_instance_call(
                errors,
                assign,
                current_line,
                current_col,
                inner_call,
                call,
            );
        } else if let Expr::Name(base) = current_expr {
            // Handle result = module.trim_customers(customers) — an
            // attribute-style call to a delegate reached via a plain
            // `import module` (see load_cross_file_symbols, which
            // populates self.param_requires by bare function name
            // regardless of how it's called at the use site). Guarded
            // on `base` not being a tracked DataFrame variable so a
            // genuine method call (`df.merge(other)`) is never treated
            // as a call to a same-named cross-file delegate function.
            if !self.variables.contains_key(base.id.as_str()) {
                let resolved = self
                    .receiver_classes
                    .get(base.id.as_str())
                    .cloned()
                    .and_then(|class| self.resolve_method_on_class(&class, attr.attr.as_str()));
                if let Some(marker_or_schema) = resolved {
                    self.bind_function_result(
                        assign,
                        &marker_or_schema,
                        attr.attr.as_str(),
                        current_line,
                        current_col,
                    );
                }
                self.check_call_requirements(call, current_line, current_col, errors);
            }
        }
    }

    fn assign_schema_instance_call(
        &mut self,
        errors: &mut Vec<LintError>,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        inner_call: &ruff_python_ast::ExprCall,
        call: &ruff_python_ast::ExprCall,
    ) {
        if let Expr::Name(schema_name) = &*inner_call.func {
            self.assign_known_schema_instance(
                errors,
                assign,
                current_line,
                current_col,
                call,
                schema_name,
            );
        }
    }

    fn assign_known_schema_instance(
        &mut self,
        errors: &mut Vec<LintError>,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        call: &ruff_python_ast::ExprCall,
        schema_name: &ruff_python_ast::ExprName,
    ) {
        if self.schemas.contains_key(schema_name.id.as_str()) {
            // Constructed from a known schema class -- always fully
            // typed, regardless of which method was chained onto it.
            self.dataframes_total += 1;
            let var_hint = assign_var_hint(assign, "df");
            self.count_typed_dataframe(
                schema_name.id.as_str(),
                &var_hint,
                current_line,
                current_col,
            );
            for target in &assign.targets {
                if let Expr::Name(target_name) = target {
                    self.variables.insert(
                        target_name.id.to_string(),
                        (schema_name.id.to_string(), current_line),
                    );
                }
            }
        } else if self.class_bases.contains_key(schema_name.id.as_str()) {
            // `Ratios().add(frame)`: a method of a class of this file.
            let targets = Self::assign_target_names(assign);
            self.apply_helper_call(&assign.value, &targets, current_line, current_col);
            self.check_call_requirements(call, current_line, current_col, errors);
        }
    }

    fn assign_subscripted_constructor(
        &mut self,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        subscript: &ruff_python_ast::ExprSubscript,
    ) {
        if let Expr::Name(name) = &*subscript.value {
            self.assign_constructor_named(assign, current_line, current_col, subscript, name);
        }
    }

    fn assign_constructor_named(
        &mut self,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        subscript: &ruff_python_ast::ExprSubscript,
        name: &ruff_python_ast::ExprName,
    ) {
        let type_name = name.id.as_str();
        if type_name == "DataFrame" {
            self.assign_schema_constructor_of(assign, current_line, current_col, subscript);
        }
    }

    fn assign_schema_constructor_of(
        &mut self,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        subscript: &ruff_python_ast::ExprSubscript,
    ) {
        if let Expr::Name(schema_name) = &*subscript.slice {
            // The constructor names the schema directly -- always
            // fully typed, with a concrete column list.
            self.dataframes_total += 1;
            let var_hint = assign_var_hint(assign, "df");
            self.count_typed_dataframe(
                schema_name.id.as_str(),
                &var_hint,
                current_line,
                current_col,
            );
            for target in &assign.targets {
                if let Expr::Name(target_name) = target {
                    self.variables.insert(
                        target_name.id.to_string(),
                        (schema_name.id.to_string(), current_line),
                    );
                }
            }
        }
    }

    fn assign_function_result(
        &mut self,
        errors: &mut Vec<LintError>,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        func_name: &ruff_python_ast::ExprName,
        call: &ruff_python_ast::ExprCall,
    ) {
        if let Some(marker_or_schema) = self
            .functions
            .get(func_name.id.as_str())
            .cloned()
            .filter(|marker| !self.summary_beats_open_marker(marker, &assign.value))
        {
            self.bind_function_result(
                assign,
                &marker_or_schema,
                func_name.id.as_str(),
                current_line,
                current_col,
            );
        } else {
            let targets = Self::assign_target_names(assign);
            self.apply_helper_call(&assign.value, &targets, current_line, current_col);
        }
        // Validate the call's first argument against the callee's
        // parameter contract, e.g. `trimmed = trim_customers(customers)`.
        self.check_call_requirements(call, current_line, current_col, errors);
    }
}

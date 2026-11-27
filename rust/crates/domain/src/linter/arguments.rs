use super::*;

impl Linter {
    // Column names handed to a frame method as arguments, and rows of a lambda a method
    // calls with the frame, checked against the frame the method is called on.
    pub(super) fn check_frame_method_arguments(
        &self,
        call: &ast::ExprCall,
        errors: &mut Vec<LintError>,
    ) {
        let Expr::Attribute(attr) = &*call.func else {
            return;
        };
        let receiver = &*attr.value;
        let at = call.range().start();
        let named = |kw: &str| {
            keyword(call, kw).and_then(|k| ast_extract::extract_string_list_or_single(&k.value))
        };
        match attr.attr.as_str() {
            "pivot_table" | "pivot" => {
                self.check_pivot_table_arguments(errors, receiver, at, named);
            }
            "melt" => {
                self.check_melt_arguments(errors, receiver, at, named);
            }
            "agg" | "aggregate" => {
                if let ControlFlow::Break(_) = self.check_agg_arguments(call, errors, receiver, at)
                {
                }
            }
            "select" | "with_columns" | "filter" => {
                self.check_filter_arguments(call, errors, attr, receiver);
            }
            "group_by" => {
                let keys = ast_extract::group_by_keys(call).unwrap_or_default();
                self.check_names_against(receiver, &keys, at, errors);
                self.check_names_against_open(receiver, &keys, at, errors);
            }
            "rename" if self.is_polars_receiver(receiver) => {
                if let Some(mapping) = ast_extract::extract_rename_mapping(call) {
                    let keys: Vec<String> = mapping.keys().cloned().collect();
                    self.check_names_against(receiver, &keys, at, errors);
                    self.check_names_against_open(receiver, &keys, at, errors);
                }
            }
            "join" if self.is_polars_receiver(receiver) => {
                if let ControlFlow::Break(_) = self.check_join_arguments(call, errors, receiver, at)
                {
                }
            }
            "merge" => {
                self.check_merge_arguments(call, errors, receiver, at, named);
            }
            "sort_values" => {
                self.check_sort_values_arguments(call, errors, receiver, at, named);
            }
            "drop_duplicates" | "unique" => {
                self.check_dedup_arguments(call, errors, receiver, at, named);
            }
            "groupby" => {
                self.check_groupby_arguments(call, errors, receiver, at, named);
            }
            "sort" => {
                self.check_sort_arguments(call, errors, receiver, at, named);
            }
            "explode" => {
                self.check_explode_arguments(call, errors, receiver, at, named);
            }
            "assign" => {
                if let ControlFlow::Break(_) = self.check_assign_arguments(call, errors, receiver) {
                }
            }
            "pipe" => {
                if let Some(Expr::Lambda(lambda)) = call.arguments.args.first() {
                    if let Some((columns, display)) = self.expr_frame_columns(receiver) {
                        self.check_lambda_rows(lambda, &columns, &display, errors);
                    }
                }
            }
            "apply" => {
                self.check_apply_arguments(call, errors, receiver);
            }
            // Bare names in the expression string are columns (or index levels, which
            // `check_column_args` steps around); extra resolvers make any name possible.
            "query" | "eval" if !self.is_polars_receiver(receiver) => {
                if let ControlFlow::Break(_) = self.check_query_arguments(call, errors, receiver) {}
            }
            _ => {}
        }
    }

    fn check_assign_arguments(
        &self,
        call: &ruff_python_ast::ExprCall,
        errors: &mut Vec<LintError>,
        receiver: &Expr,
    ) -> ControlFlow<()> {
        let Some((mut columns, display)) = self.expr_frame_columns(receiver) else {
            return ControlFlow::Break(());
        };
        // A keyword may read the columns added by the ones before it.
        for kw in &call.arguments.keywords {
            self.check_assign_lambda(errors, &mut columns, &display, kw);
        }
        ControlFlow::Continue(())
    }

    fn check_join_arguments(
        &self,
        call: &ruff_python_ast::ExprCall,
        errors: &mut Vec<LintError>,
        receiver: &Expr,
        at: ruff_text_size::TextSize,
    ) -> ControlFlow<()> {
        let Some(right) = call
            .arguments
            .args
            .first()
            .or_else(|| keyword(call, "other").map(|k| &k.value))
        else {
            return ControlFlow::Break(());
        };
        self.check_polars_join(call, errors, receiver, at, right);
        ControlFlow::Continue(())
    }

    fn check_agg_arguments(
        &self,
        call: &ruff_python_ast::ExprCall,
        errors: &mut Vec<LintError>,
        receiver: &Expr,
        at: ruff_text_size::TextSize,
    ) -> ControlFlow<()> {
        let group_attr = match check_grouped_aggregation(receiver) {
            Some(value) => value,
            None => return ControlFlow::Break(()),
        };
        let sources = match self.check_named_aggregation(call, errors, at, group_attr) {
            Some(value) => value,
            None => return ControlFlow::Break(()),
        };
        self.check_names_against_closed(&group_attr.value, &sources, at, errors);
        ControlFlow::Continue(())
    }

    fn check_query_arguments(
        &self,
        call: &ruff_python_ast::ExprCall,
        errors: &mut Vec<LintError>,
        receiver: &Expr,
    ) -> ControlFlow<()> {
        if let ControlFlow::Break(_) = check_polars_query(call) {
            return ControlFlow::Break(());
        }
        let Some(arg) = call
            .arguments
            .args
            .first()
            .or_else(|| keyword(call, "expr").map(|k| &k.value))
        else {
            return ControlFlow::Break(());
        };
        let Some(names) =
            ast_extract::extract_string_literal(arg).and_then(query_names::query_names)
        else {
            return ControlFlow::Break(());
        };
        self.check_column_args(receiver, &names.reads, arg.range().start(), errors);
        ControlFlow::Continue(())
    }

    fn check_melt_arguments(
        &self,
        errors: &mut Vec<LintError>,
        receiver: &Expr,
        at: ruff_text_size::TextSize,
        named: impl Fn(&str) -> Option<Vec<String>>,
    ) {
        let names: Vec<String> = ["id_vars", "value_vars"]
            .iter()
            .filter_map(|kw| named(kw))
            .flatten()
            .collect();
        self.check_names_against_closed(receiver, &names, at, errors);
    }

    fn check_assign_lambda(
        &self,
        errors: &mut Vec<LintError>,
        columns: &mut Vec<String>,
        display: &str,
        kw: &ruff_python_ast::Keyword,
    ) {
        if let Expr::Lambda(lambda) = &kw.value {
            self.check_lambda_rows(lambda, &*columns, display, errors);
        }
        if let Some(name) = &kw.arg {
            columns.push(name.to_string());
        }
    }

    fn check_pivot_table_arguments(
        &self,
        errors: &mut Vec<LintError>,
        receiver: &Expr,
        at: ruff_text_size::TextSize,
        named: impl Fn(&str) -> Option<Vec<String>>,
    ) {
        let names: Vec<String> = ["index", "columns", "values"]
            .iter()
            .filter_map(|kw| named(kw))
            .flatten()
            .collect();
        self.check_names_against_closed(receiver, &names, at, errors);
    }

    fn check_groupby_arguments(
        &self,
        call: &ruff_python_ast::ExprCall,
        errors: &mut Vec<LintError>,
        receiver: &Expr,
        at: ruff_text_size::TextSize,
        named: impl Fn(&str) -> Option<Vec<String>>,
    ) {
        let by = call
            .arguments
            .args
            .first()
            .and_then(ast_extract::extract_string_list_or_single)
            .or_else(|| named("by"));
        self.check_column_args(receiver, &by.unwrap_or_default(), at, errors);
    }
}

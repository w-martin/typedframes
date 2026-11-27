use super::*;

impl Linter {
    pub(super) fn check_dedup_arguments(
        &self,
        call: &ruff_python_ast::ExprCall,
        errors: &mut Vec<LintError>,
        receiver: &Expr,
        at: ruff_text_size::TextSize,
        named: impl Fn(&str) -> Option<Vec<String>>,
    ) {
        let subset = named("subset").or_else(|| {
            call.arguments
                .args
                .first()
                .and_then(ast_extract::extract_string_list_or_single)
        });
        self.check_column_args(receiver, &subset.unwrap_or_default(), at, errors);
    }

    pub(super) fn check_sort_values_arguments(
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

    pub(super) fn check_sort_arguments(
        &self,
        call: &ruff_python_ast::ExprCall,
        errors: &mut Vec<LintError>,
        receiver: &Expr,
        at: ruff_text_size::TextSize,
        named: impl Fn(&str) -> Option<Vec<String>>,
    ) {
        let mut names: Vec<String> = call
            .arguments
            .args
            .iter()
            .filter_map(ast_extract::extract_string_list_or_single)
            .flatten()
            .collect();
        names.extend(named("by").unwrap_or_default());
        self.check_column_args(receiver, &names, at, errors);
    }

    pub(super) fn check_explode_arguments(
        &self,
        call: &ruff_python_ast::ExprCall,
        errors: &mut Vec<LintError>,
        receiver: &Expr,
        at: ruff_text_size::TextSize,
        named: impl Fn(&str) -> Option<Vec<String>>,
    ) {
        let mut names: Vec<String> = call
            .arguments
            .args
            .iter()
            .filter_map(ast_extract::extract_string_list_or_single)
            .flatten()
            .collect();
        names.extend(named("column").unwrap_or_default());
        names.extend(named("columns").unwrap_or_default());
        self.check_column_args(receiver, &names, at, errors);
    }

    pub(super) fn check_apply_arguments(
        &self,
        call: &ruff_python_ast::ExprCall,
        errors: &mut Vec<LintError>,
        receiver: &Expr,
    ) {
        let by_row = matches!(
            keyword(call, "axis").map(|k| &k.value),
            Some(Expr::NumberLiteral(n)) if matches!(&n.value, ast::Number::Int(i) if i.as_u64() == Some(1))
        ) || matches!(
            keyword(call, "axis").map(|k| &k.value),
            Some(Expr::StringLiteral(s)) if s.value.to_str() == "columns"
        );
        if let (true, Some(Expr::Lambda(lambda))) = (by_row, call.arguments.args.first()) {
            if let Some((columns, display)) = self.expr_frame_columns(receiver) {
                self.check_lambda_rows(lambda, &columns, &display, errors);
            }
        }
    }

    pub(super) fn check_polars_join(
        &self,
        call: &ruff_python_ast::ExprCall,
        errors: &mut Vec<LintError>,
        receiver: &Expr,
        at: ruff_text_size::TextSize,
        right: &Expr,
    ) {
        let keys = |kw: &str| {
            keyword(call, kw)
                .and_then(|k| ast_extract::literal_key_columns(&k.value))
                .unwrap_or_default()
        };
        let on = keys("on");
        self.check_names_against(
            receiver,
            &[on.clone(), keys("left_on")].concat(),
            at,
            errors,
        );
        self.check_names_against(right, &[on, keys("right_on")].concat(), at, errors);
    }

    pub(super) fn check_merge_arguments(
        &self,
        call: &ruff_python_ast::ExprCall,
        errors: &mut Vec<LintError>,
        receiver: &Expr,
        at: ruff_text_size::TextSize,
        named: impl Fn(&str) -> Option<Vec<String>>,
    ) {
        let (left, right) = match (receiver, &*call.arguments.args) {
            (Expr::Name(module), [left, right, ..])
                if !self.variables.contains_key(module.id.as_str())
                    && !matches!(right, Expr::StringLiteral(_)) =>
            {
                (left, Some(right))
            }
            (_, [right, ..]) => (receiver, Some(right)),
            _ => (receiver, keyword(call, "right").map(|k| &k.value)),
        };
        let on = named("on").unwrap_or_default();
        let left_on = named("left_on").unwrap_or_default();
        let right_on = named("right_on").unwrap_or_default();
        self.check_column_args(left, &[on.clone(), left_on].concat(), at, errors);
        if let Some(right) = right {
            self.check_column_args(right, &[on, right_on].concat(), at, errors);
        }
    }

    pub(super) fn check_filter_arguments(
        &self,
        call: &ruff_python_ast::ExprCall,
        errors: &mut Vec<LintError>,
        attr: &ruff_python_ast::ExprAttribute,
        receiver: &Expr,
    ) {
        let literal = attr.attr.as_str() == "select";
        // Each argument reports at its own position, so a long chain points at the
        // expression that reads the missing name.
        for arg in call
            .arguments
            .args
            .iter()
            .chain(call.arguments.keywords.iter().map(|k| &k.value))
        {
            let mut names = ast_extract::collect_pl_col_names(arg);
            if literal {
                names.extend(ast_extract::extract_string_list_or_single(arg).unwrap_or_default());
            }
            let at = arg.range().start();
            self.check_names_against(receiver, &names, at, errors);
            self.check_names_against_open(receiver, &names, at, errors);
        }
        if attr.attr.as_str() == "with_columns" && self.expr_frame_columns(receiver).is_none() {
            self.check_sibling_reads(receiver, call, errors);
        }
    }

    pub(super) fn check_named_aggregation(
        &self,
        call: &ruff_python_ast::ExprCall,
        errors: &mut Vec<LintError>,
        at: ruff_text_size::TextSize,
        group_attr: &ruff_python_ast::ExprAttribute,
    ) -> Option<Vec<String>> {
        if group_attr.attr.as_str() == "group_by" {
            let names: Vec<String> = call
                .arguments
                .args
                .iter()
                .chain(call.arguments.keywords.iter().map(|k| &k.value))
                .flat_map(ast_extract::collect_pl_col_names)
                .collect();
            self.check_names_against(&group_attr.value, &names, at, errors);
            self.check_names_against_open(&group_attr.value, &names, at, errors);
            return None;
        }
        if group_attr.attr.as_str() != "groupby" {
            return None;
        }
        let sources: Vec<String> = call
            .arguments
            .keywords
            .iter()
            .filter_map(|k| match &k.value {
                Expr::Tuple(pair) => match pair.elts.first() {
                    Some(Expr::StringLiteral(s)) => Some(s.value.to_str().to_string()),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        Some(sources)
    }
}

pub(super) fn check_polars_query(call: &ruff_python_ast::ExprCall) -> ControlFlow<()> {
    if ["local_dict", "global_dict", "resolvers"]
        .iter()
        .any(|kw| keyword(call, kw).is_some())
    {
        return ControlFlow::Break(());
    }
    ControlFlow::Continue(())
}

pub(super) fn check_grouped_aggregation(
    receiver: &Expr,
) -> Option<&ruff_python_ast::ExprAttribute> {
    let Expr::Call(grouped) = receiver else {
        return None;
    };
    let Expr::Attribute(group_attr) = &*grouped.func else {
        return None;
    };
    Some(group_attr)
}

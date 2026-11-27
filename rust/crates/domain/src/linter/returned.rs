use super::*;

impl Linter {
    // The columns an expression evaluates to, when they follow from a resolved frame
    // and edits whose arguments are literal; `None` as soon as any step is not understood.
    pub(super) fn returned_expr_columns(&self, expr: &Expr) -> Option<Vec<String>> {
        match expr {
            Expr::Name(name) => {
                let (schema, _) = self.variables.get(name.id.as_str())?;
                if self.schema_is_unresolved(schema) {
                    return None;
                }
                self.schemas.get(schema).cloned()
            }
            Expr::Subscript(subscript) => self.returned_subscript_columns(subscript),
            Expr::Call(call) => self.returned_call_columns(call),
            _ => None,
        }
    }

    fn returned_call_columns(&self, call: &ruff_python_ast::ExprCall) -> Option<Vec<String>> {
        if let Some(columns) = self.function_result_columns(call) {
            return Some(columns);
        }
        self.returned_method_columns(call)
    }

    fn returned_method_columns(&self, call: &ruff_python_ast::ExprCall) -> Option<Vec<String>> {
        let Expr::Attribute(attr) = &*call.func else {
            return None;
        };
        self.returned_method_columns_of(call, attr)
    }

    fn returned_method_columns_of(
        &self,
        call: &ruff_python_ast::ExprCall,
        attr: &ruff_python_ast::ExprAttribute,
    ) -> Option<Vec<String>> {
        let method = attr.attr.as_str();
        self.returned_known_method_columns(call, attr, method)
    }

    fn returned_known_method_columns(
        &self,
        call: &ruff_python_ast::ExprCall,
        attr: &ruff_python_ast::ExprAttribute,
        method: &str,
    ) -> Option<Vec<String>> {
        if let Some(value) = self.returned_function_call_columns(call, method) {
            return value;
        }
        let from_sql = |sql: Option<String>| match sql::columns_from_select(&sql?, self.sql_dialect)
        {
            sql::SqlOutcome::Columns(cols) => Some(cols),
            sql::SqlOutcome::Wildcard | sql::SqlOutcome::Unparsed => None,
        };
        if let Some(value) = self.returned_sql_columns(call, attr, method, from_sql) {
            return value;
        }
        if method == "agg" {
            if let Expr::Call(group) = &*attr.value {
                if matches!(&*group.func, Expr::Attribute(g) if g.attr.as_str() == "group_by") {
                    let mut columns = ast_extract::group_by_keys(group)?;
                    columns.extend(self.with_columns_outputs(call)?);
                    return Some(columns);
                }
                // `group_by_dynamic("t", every=.., group_by=..)`: the time column, the
                // keys, then the aggregates.
                if matches!(&*group.func, Expr::Attribute(g) if g.attr.as_str() == "group_by_dynamic")
                {
                    let columns = self.returned_attribute_columns(call, group)?;
                    return Some(columns);
                }
            }
        }
        // `pl.from_pandas(frame)` carries the columns of `frame`.
        self.returned_reshaping_method_columns(call, attr, method)
    }

    fn returned_reshaping_method_columns(
        &self,
        call: &ruff_python_ast::ExprCall,
        attr: &ruff_python_ast::ExprAttribute,
        method: &str,
    ) -> Option<Vec<String>> {
        if method == "from_pandas"
            && matches!(&*attr.value, Expr::Name(m) if LOAD_MODULES.contains(&m.id.as_str()))
        {
            return self.returned_expr_columns(call.arguments.args.first()?);
        }
        if method == "select" {
            return Self::selected_columns(call);
        }
        if matches!(method, "agg" | "aggregate") {
            return Self::series_agg_list(call, false)
                .or_else(|| Self::named_agg_columns(call, false));
        }
        if let Some(columns) = Self::groupby_selection_aggregate(call, false) {
            return Some(columns);
        }
        if method == "to_frame" {
            if let Some(decided) = multi_index_frame_columns(call, attr) {
                return decided;
            }
        }
        if method == "to_frame" {
            return match call
                .arguments
                .args
                .first()
                .or_else(|| keyword(call, "name").map(|k| &k.value))
            {
                Some(name) => Some(vec![ast_extract::extract_string_literal(name)?.to_string()]),
                None => {
                    self.series_index(&attr.value)?;
                    Some(vec![self.series_name(&attr.value)?])
                }
            };
        }
        if method == "reset_index" && call.arguments.args.is_empty() {
            if let Some(decided) = Self::named_series_reset_columns(call, attr) {
                return decided;
            }
        }
        self.returned_reset_index_columns(call, attr, method)
    }

    // `groupby(keys)["v"].nunique().reset_index(name="n")`: the keys, then the Series under its
    // new name (`Some` once the call has that shape, even when they can't be read).
    fn named_series_reset_columns(
        call: &ast::ExprCall,
        attr: &ast::ExprAttribute,
    ) -> Option<Option<Vec<String>>> {
        let ([kw], Expr::Call(inner)) = (&*call.arguments.keywords, &*attr.value) else {
            return None;
        };
        if kw.arg.as_ref().is_none_or(|a| a.as_str() != "name") {
            return None;
        }
        Some(
            ast_extract::extract_string_literal(&kw.value).and_then(|name| {
                let mut columns = Self::series_groupby_reduction_keys(inner)?;
                columns.push(name.to_string());
                Some(columns)
            }),
        )
    }

    fn returned_reset_index_columns(
        &self,
        call: &ruff_python_ast::ExprCall,
        attr: &ruff_python_ast::ExprAttribute,
        method: &str,
    ) -> Option<Vec<String>> {
        if let Some(decided) = Self::reset_index_aggregate(call, attr, method) {
            return decided;
        }
        self.returned_index_reset_columns(call, attr, method)
    }

    // `<aggregate>.reset_index()`: the aggregate's columns, when it is one this checker reads
    // (`Some` even when they can't be read); `None` to keep looking.
    fn reset_index_aggregate(
        call: &ruff_python_ast::ExprCall,
        attr: &ruff_python_ast::ExprAttribute,
        method: &str,
    ) -> Option<Option<Vec<String>>> {
        if method != "reset_index"
            || !call.arguments.args.is_empty()
            || !call.arguments.keywords.is_empty()
        {
            return None;
        }
        let Expr::Call(inner) = &*attr.value else {
            return None;
        };
        if let Some(columns) = Self::series_agg_list(inner, true) {
            return Some(Some(columns));
        }
        let Expr::Attribute(inner_attr) = &*inner.func else {
            return None;
        };
        match inner_attr.attr.as_str() {
            "agg" | "aggregate" => Some(Self::named_agg_columns(inner, true)),
            _ if Self::groupby_selection_aggregate(inner, true).is_some() => {
                Some(Self::groupby_selection_aggregate(inner, true))
            }
            "pivot_table" => Some(Self::pivot_table_columns(inner, true)),
            _ => None,
        }
    }

    fn returned_index_reset_columns(
        &self,
        call: &ruff_python_ast::ExprCall,
        attr: &ruff_python_ast::ExprAttribute,
        method: &str,
    ) -> Option<Vec<String>> {
        if method == "reset_index"
            && call.arguments.args.is_empty()
            && call.arguments.keywords.is_empty()
        {
            if let Expr::Name(name) = &*attr.value {
                let (schema, _) = self.variables.get(name.id.as_str())?;
                let mut columns = self.schema_index.get(schema)?.clone();
                columns.extend(self.schemas.get(schema)?.iter().cloned());
                return Some(columns);
            }
        }
        self.returned_derived_columns(call, attr, method)
    }

    fn returned_derived_columns(
        &self,
        call: &ruff_python_ast::ExprCall,
        attr: &ruff_python_ast::ExprAttribute,
        method: &str,
    ) -> Option<Vec<String>> {
        if let Some(value) = self.returned_pivot_table_columns(call, attr, method) {
            return value;
        }
        // `groupby(..).tail(n)` keeps every row-wise column, keys included.
        let receiver = match (method, &*attr.value) {
            ("tail" | "head", Expr::Call(inner)) => match &*inner.func {
                Expr::Attribute(g) if g.attr.as_str() == "groupby" => &*g.value,
                _ => &*attr.value,
            },
            _ => &*attr.value,
        };
        let base = self.returned_expr_columns(receiver)?;
        // Without `columns=` or `axis=`, pandas reads `drop(x)` and `rename({..})` as
        // row-label operations, which leave the columns alone.
        match method {
            "set_index" if keyword(call, "inplace").is_none() => {
                returned_set_index_columns(call, &base)
            }
            "rename" if keyword(call, "columns").is_some() => {
                let mapping = ast_extract::extract_rename_mapping(call)?;
                apply_edits(&base, &[ColumnEdit::Rename(mapping)])
            }
            "drop" if keyword(call, "columns").is_some() || keyword(call, "axis").is_some() => {
                let dropped = ast_extract::extract_drop_columns(call, false)?;
                apply_edits(&base, &[ColumnEdit::Remove(dropped)])
            }
            // Polars spells these positionally, which pandas reads as row labels.
            "rename" if self.is_polars_receiver(receiver) => {
                let mapping = ast_extract::extract_rename_mapping(call)?;
                apply_edits(&base, &[ColumnEdit::Rename(mapping)])
            }
            "drop" if self.is_polars_receiver(receiver) => {
                let mut dropped = Vec::new();
                for arg in &call.arguments.args {
                    dropped.extend(ast_extract::extract_string_list_or_single(arg)?);
                }
                apply_edits(&base, &[ColumnEdit::Remove(dropped)])
            }
            "add_prefix" | "add_suffix" => returned_affixed_columns(call, method, &base),
            "merge" => self.returned_merge_columns(call, &base),
            "join" if self.is_polars_receiver(receiver) => self.returned_join_columns(call, &base),
            "with_columns" => {
                let added = self.with_columns_outputs(call)?;
                apply_edits(&base, &[ColumnEdit::Add(added)])
            }
            // Polars: one new column, named by the first argument or `name=`.
            "with_row_index" => returned_row_index_columns(call, &base),
            // Polars: the same columns at a finer time grid.
            "upsample" => Some(base),
            "rename" | "drop" => None,
            // Keeps the old index as a column unless it is dropped.
            "reset_index" => matches!(
                keyword(call, "drop").map(|k| &k.value),
                Some(Expr::BooleanLiteral(b)) if b.value
            )
            .then_some(base),
            "assign" => {
                let added = ast_extract::assign_output_names(call)?;
                apply_edits(&base, &[ColumnEdit::Add(added)])
            }
            // Without an assignment `eval` returns a Series.
            "eval" if !self.is_polars_receiver(receiver) => returned_eval_columns(call, &base),
            _ if ROW_PASSTHROUGH_METHODS.contains(&method)
                || SCHEMA_PRESERVING_METHODS.contains(&method) =>
            {
                Some(base)
            }
            _ => None,
        }
    }
}

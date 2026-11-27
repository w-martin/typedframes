use super::*;

impl Linter {
    pub(super) fn returned_pivot_table_columns(
        &self,
        call: &ruff_python_ast::ExprCall,
        attr: &ruff_python_ast::ExprAttribute,
        method: &str,
    ) -> Option<Option<Vec<String>>> {
        if method == "pivot_table" {
            return Some(Self::pivot_table_columns(call, false));
        }
        if matches!(method, "melt" | "unpivot") {
            return Some(self.melted_columns(&attr.value, call));
        }
        None
    }

    pub(super) fn returned_join_columns(
        &self,
        call: &ruff_python_ast::ExprCall,
        base: &[String],
    ) -> Option<Vec<String>> {
        let right = call
            .arguments
            .args
            .first()
            .or_else(|| keyword(call, "other").map(|k| &k.value))?;
        let right = self.returned_expr_columns(right)?;
        Self::polars_join(base, &right, call).map(|join| join.columns)
    }

    pub(super) fn returned_merge_columns(
        &self,
        call: &ruff_python_ast::ExprCall,
        base: &[String],
    ) -> Option<Vec<String>> {
        let right = call
            .arguments
            .args
            .first()
            .or_else(|| keyword(call, "right").map(|k| &k.value))?;
        let right = self.returned_expr_columns(right)?;
        match merged_columns(base, &right, call) {
            Merged::Columns(columns) => Some(columns),
            Merged::Unknown => None,
        }
    }

    pub(super) fn returned_attribute_columns(
        &self,
        call: &ruff_python_ast::ExprCall,
        group: &ruff_python_ast::ExprCall,
    ) -> Option<Vec<String>> {
        let mut columns = vec![ast_extract::extract_string_literal(
            group
                .arguments
                .args
                .first()
                .or_else(|| keyword(group, "index_column").map(|k| &k.value))?,
        )?
        .to_string()];
        if let Some(by) = keyword(group, "group_by") {
            columns.extend(ast_extract::literal_key_columns(&by.value)?);
        }
        columns.extend(self.with_columns_outputs(call)?);
        Some(columns)
    }

    pub(super) fn returned_subscript_columns(
        &self,
        subscript: &ruff_python_ast::ExprSubscript,
    ) -> Option<Vec<String>> {
        if let Expr::Attribute(indexer) = &*subscript.value {
            if matches!(indexer.attr.as_str(), "loc" | "iloc") {
                return self.indexer_columns(&indexer.value, &subscript.slice);
            }
        }
        // A row mask or slice keeps the columns.
        if Self::is_row_mask(&subscript.slice) {
            return self.returned_expr_columns(&subscript.value);
        }
        // A literal list of names selects exactly those columns, whatever the
        // frame it is taken from held.
        ast_extract::extract_string_list(&subscript.slice)
    }

    pub(super) fn returned_sql_columns(
        &self,
        call: &ruff_python_ast::ExprCall,
        attr: &ruff_python_ast::ExprAttribute,
        method: &str,
        from_sql: impl Fn(Option<String>) -> Option<Vec<String>>,
    ) -> Option<Option<Vec<String>>> {
        if SQL_FINALIZE_METHODS.contains(&method) {
            // `to_pandas` of anything else keeps the frame's columns, below.
            if let Some(inner) = ast_extract::sql_producing_call(&attr.value) {
                return Some(from_sql(self.extract_sql_literal(inner)));
            }
        }
        if method == "fetch_pandas_all" {
            let Expr::Name(cursor) = &*attr.value else {
                return Some(None);
            };
            return Some(from_sql(self.cursor_sql.get(cursor.id.as_str()).cloned()));
        }
        // `inplace=True` returns None, not a frame.
        if keyword(call, "inplace").is_some() {
            return Some(None);
        }
        None
    }

    pub(super) fn returned_function_call_columns(
        &self,
        call: &ruff_python_ast::ExprCall,
        method: &str,
    ) -> Option<Option<Vec<String>>> {
        if is_load_callee(&call.func) {
            // A column list that isn't literal says nothing about the columns.
            let literal_lists = call
                .arguments
                .keywords
                .iter()
                .filter(|k| {
                    k.arg
                        .as_ref()
                        .is_some_and(|a| matches!(a.as_str(), "columns" | "usecols"))
                })
                .all(|k| ast_extract::extract_string_list(&k.value).is_some());
            return Some(if literal_lists {
                self.extract_load_columns(method, call).0
            } else {
                None
            });
        }
        None
    }
}

pub(super) fn returned_eval_columns(
    call: &ruff_python_ast::ExprCall,
    base: &[String],
) -> Option<Vec<String>> {
    let text = ast_extract::extract_string_literal(call.arguments.args.first()?)?;
    let assigned = query_names::query_names(text)?.assigned;
    if assigned.is_empty() {
        return None;
    }
    apply_edits(base, &[ColumnEdit::Add(assigned)])
}

pub(super) fn returned_set_index_columns(
    call: &ruff_python_ast::ExprCall,
    base: &[String],
) -> Option<Vec<String>> {
    let dropped = match keyword(call, "drop").map(|k| &k.value) {
        Some(Expr::BooleanLiteral(b)) if !b.value => Vec::new(),
        Some(_) => return None,
        None => ast_extract::extract_string_list_or_single(call.arguments.args.first()?)?,
    };
    apply_edits(base, &[ColumnEdit::Remove(dropped)])
}

pub(super) fn returned_row_index_columns(
    call: &ruff_python_ast::ExprCall,
    base: &[String],
) -> Option<Vec<String>> {
    let name = match call.arguments.args.first() {
        Some(arg) => ast_extract::extract_string_literal(arg)?.to_string(),
        None => match keyword(call, "name") {
            Some(k) => ast_extract::extract_string_literal(&k.value)?.to_string(),
            None => "index".to_string(),
        },
    };
    apply_edits(base, &[ColumnEdit::Add(vec![name])])
}

pub(super) fn returned_affixed_columns(
    call: &ruff_python_ast::ExprCall,
    method: &str,
    base: &[String],
) -> Option<Vec<String>> {
    let affix = ast_extract::extract_string_literal(call.arguments.args.first()?)?;
    Some(
        base.iter()
            .map(|c| {
                if method == "add_prefix" {
                    format!("{affix}{c}")
                } else {
                    format!("{c}{affix}")
                }
            })
            .collect(),
    )
}

// `pd.MultiIndex.from_product(.., names=[..]).to_frame()`: one column per level (`Some` once the
// call has that shape, even when the names can't be read).
pub(super) fn multi_index_frame_columns(
    call: &ast::ExprCall,
    attr: &ast::ExprAttribute,
) -> Option<Option<Vec<String>>> {
    if keyword(call, "name").is_some() {
        return None;
    }
    let Expr::Call(build) = &*attr.value else {
        return None;
    };
    let Expr::Attribute(ctor) = &*build.func else {
        return None;
    };
    let multi_index = matches!(
        ctor.attr.as_str(),
        "from_product" | "from_arrays" | "from_tuples"
    ) && matches!(&*ctor.value, Expr::Attribute(m) if m.attr.as_str() == "MultiIndex");
    if !multi_index {
        return None;
    }
    Some(keyword(build, "names").and_then(|k| ast_extract::extract_string_list_or_single(&k.value)))
}

use super::*;

impl Linter {
    // `frame[<this>]` selecting rows: a comparison, a combination of masks, a mask method,
    // or a row slice. A bare name could be a column list, so it is not one.
    pub(super) fn is_row_mask(slice: &Expr) -> bool {
        match slice {
            Expr::Compare(_) | Expr::Slice(_) => true,
            Expr::UnaryOp(op) => {
                matches!(op.op, ast::UnaryOp::Invert | ast::UnaryOp::Not)
                    && Self::is_row_mask(&op.operand)
                    || matches!(op.op, ast::UnaryOp::Invert)
                        && matches!(&*op.operand, Expr::Call(_) | Expr::Subscript(_))
            }
            Expr::BinOp(bin) => {
                matches!(
                    bin.op,
                    ast::Operator::BitAnd | ast::Operator::BitOr | ast::Operator::BitXor
                ) && (Self::is_row_mask(&bin.left) || Self::is_row_mask(&bin.right))
            }
            Expr::Call(call) => matches!(&*call.func, Expr::Attribute(a) if matches!(
                a.attr.as_str(),
                "isin" | "notna" | "notnull" | "isna" | "isnull" | "between" | "duplicated"
                    | "contains" | "startswith" | "endswith" | "match"
            )),
            _ => false,
        }
    }

    // The columns a polars `select(...)` leaves, whatever the frame held.
    pub(super) fn selected_columns(call: &ast::ExprCall) -> Option<Vec<String>> {
        let mut columns = match &*call.arguments.args {
            [list @ Expr::List(_)] => ast_extract::extract_string_list(list)?,
            args => args
                .iter()
                .map(|arg| {
                    ast_extract::extract_string_literal(arg)
                        .map(str::to_string)
                        .or_else(|| ast_extract::pl_expr_output_name(arg))
                })
                .collect::<Option<Vec<_>>>()?,
        };
        columns.extend(ast_extract::assign_output_names(call)?);
        Some(columns)
    }

    // The columns of the frame a call to a function or method of this file returns, when
    // its body made them known.
    pub(super) fn function_result_columns(&self, call: &ast::ExprCall) -> Option<Vec<String>> {
        let schema = match &*call.func {
            Expr::Name(name) => self.functions.get(name.id.as_str())?.clone(),
            Expr::Attribute(attr) if matches!(&*attr.value, Expr::Name(n) if matches!(n.id.as_str(), "self" | "cls")) => {
                self.resolve_self_method(attr.attr.as_str())?
            }
            _ => return None,
        };
        if schema == OPEN_FRAME_MARKER || self.schema_is_unresolved(&schema) {
            return None;
        }
        self.schemas.get(&schema).cloned()
    }

    // The schema of one operand of a merge or concat: a tracked name, or an expression whose
    // columns follow from literal edits of a frame (`feats[["a", "b"]]`, `x.copy()`).
    pub(super) fn operand_schema(&mut self, expr: &Expr, line: usize) -> Option<String> {
        if let Expr::Name(name) = expr {
            return self.variables.get(name.id.as_str()).map(|(s, _)| s.clone());
        }
        let columns = self.returned_expr_columns(expr)?;
        Some(self.make_inferred_schema(columns, "operand", line))
    }

    // The schema of every frame a `concat` joins, or `None` unless all of them are known:
    // one the checker cannot see makes the result's columns unknowable.
    pub(super) fn concat_schemas(
        &mut self,
        call: &ast::ExprCall,
        line: usize,
    ) -> Option<Vec<String>> {
        let objs = call
            .arguments
            .args
            .first()
            .or_else(|| keyword(call, "objs").map(|k| &k.value))?;
        let (Expr::List(ast::ExprList { elts, .. }) | Expr::Tuple(ast::ExprTuple { elts, .. })) =
            objs
        else {
            return None;
        };
        if elts.len() < 2 {
            return None;
        }
        elts.iter().map(|e| self.operand_schema(e, line)).collect()
    }

    // `melt`/`unpivot` keep the id columns and add the variable and value columns, whatever
    // the melted ones are called.
    pub(super) fn melted_columns(
        &self,
        receiver: &Expr,
        call: &ast::ExprCall,
    ) -> Option<Vec<String>> {
        let unpivot = matches!(&*call.func, Expr::Attribute(a) if a.attr.as_str() == "unpivot");
        let (ids, var, value) = match (unpivot, self.is_polars_receiver(receiver)) {
            (true, _) => ("index", "variable_name", "value_name"),
            // The older polars spelling.
            (false, true) => ("id_vars", "variable_name", "value_name"),
            (false, false) => ("id_vars", "var_name", "value_name"),
        };
        let given = |kw: &str, default: &str| match keyword(call, kw) {
            None => Some(default.to_string()),
            Some(k) => ast_extract::extract_string_literal(&k.value).map(str::to_string),
        };
        let mut columns = match keyword(call, ids) {
            None => Vec::new(),
            Some(k) => ast_extract::extract_string_list_or_single(&k.value)?,
        };
        columns.push(given(var, "variable")?);
        columns.push(given(value, "value")?);
        Some(columns)
    }

    // `df.pivot_table(index=.., values=..)` with no `columns=`: the values are the columns,
    // and the index names join them once reset.
    pub(super) fn pivot_table_columns(
        call: &ast::ExprCall,
        reset_index: bool,
    ) -> Option<Vec<String>> {
        if keyword(call, "columns").is_some()
            || keyword(call, "margins").is_some()
            || !call.arguments.args.is_empty()
        {
            return None;
        }
        let values = ast_extract::extract_string_list_or_single(&keyword(call, "values")?.value)?;
        if !reset_index {
            return Some(values);
        }
        let mut columns =
            ast_extract::extract_string_list_or_single(&keyword(call, "index")?.value)?;
        columns.extend(values);
        Some(columns)
    }

    // `base.loc[..]` / `base.iloc[..]`: a row filter keeps the columns, and a `[rows, [names]]`
    // pair picks exactly those. A scalar label yields a row (a Series), not a frame.
    pub(super) fn indexer_columns(&self, base: &Expr, slice: &Expr) -> Option<Vec<String>> {
        match slice {
            Expr::Tuple(pair) => match pair.elts.as_slice() {
                [_, columns] => ast_extract::extract_string_list(columns),
                _ => None,
            },
            Expr::StringLiteral(_) | Expr::NumberLiteral(_) | Expr::List(_) => None,
            _ => self.returned_expr_columns(base),
        }
    }

    // Whether `schema_name` is a recognized DataFrame origin with no concrete column
    // list (see `unresolved_schemas`'s docs) — checked by every column-access
    // validator BEFORE consulting `schema_has_column`, since an unresolved schema
    // means "unverifiable", a third outcome distinct from both "found" and "missing".
    pub(super) fn schema_is_unresolved(&self, schema_name: &str) -> bool {
        self.unresolved_schemas.contains(schema_name)
    }

    // Column membership check used by every column-access validator (see its call
    // sites, all of which check `schema_is_unresolved` first). Exact match:
    // SQL-derived schemas already have `self.sql_dialect`'s case-folding baked into
    // their column names by `sql::columns_from_select` at inference time, so e.g. a
    // Snowflake query genuinely produces `ORDER_ID`, and `df["order_id"]` is a real
    // bug worth reporting, not a false positive to suppress. Kept as a named helper
    // (rather than inlining `cols.iter().any(|c| c == col)` at each call site) so
    // every validator agrees by construction if this ever needs to change again.
    pub(super) fn schema_has_column(&self, schema_name: &str, col: &str) -> bool {
        self.schemas
            .get(schema_name)
            .is_some_and(|cols| cols.iter().any(|c| c == col))
    }
}

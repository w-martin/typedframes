use super::*;

impl Linter {
    // `frame.group_by(keys).agg(...)` and `frame.join(other, ...)` on resolved polars
    // frames, when the result's columns follow from literal arguments.
    pub(super) fn polars_frame_op(
        &self,
        attr: &ast::ExprAttribute,
        call: &ast::ExprCall,
    ) -> Option<PolarsFrameOp> {
        let columns_of = |name: &str| -> Option<(String, Vec<String>)> {
            let (schema, _) = self.variables.get(name)?;
            if self.schema_is_unresolved(schema) {
                return None;
            }
            Some((schema.clone(), self.schemas.get(schema)?.clone()))
        };
        match attr.attr.as_str() {
            "agg" => {
                let Expr::Call(group) = &*attr.value else {
                    return None;
                };
                let Expr::Attribute(group_attr) = &*group.func else {
                    return None;
                };
                if group_attr.attr.as_str() != "group_by" {
                    return None;
                }
                let Expr::Name(recv) = &*group_attr.value else {
                    return None;
                };
                columns_of(recv.id.as_str())?;
                let keys = ast_extract::group_by_keys(group)?;
                let aggregated = self.with_columns_outputs(call)?;
                let mut reads = keys.clone();
                reads.extend(
                    call.arguments
                        .args
                        .iter()
                        .chain(call.arguments.keywords.iter().map(|k| &k.value))
                        .flat_map(ast_extract::collect_pl_col_names),
                );
                let mut columns = keys;
                for name in aggregated {
                    if !columns.contains(&name) {
                        columns.push(name);
                    }
                }
                Some(PolarsFrameOp {
                    reads: vec![(recv.id.to_string(), reads)],
                    columns,
                })
            }
            "join" if self.is_polars_receiver(&attr.value) => {
                let Expr::Name(left) = &*attr.value else {
                    return None;
                };
                let right_expr = call
                    .arguments
                    .args
                    .first()
                    .or_else(|| keyword(call, "other").map(|k| &k.value))?;
                let Expr::Name(right) = right_expr else {
                    return None;
                };
                let (_, left_cols) = columns_of(left.id.as_str())?;
                let (_, right_cols) = columns_of(right.id.as_str())?;
                let join = Self::polars_join(&left_cols, &right_cols, call)?;
                let reads = vec![
                    (left.id.to_string(), join.left_keys),
                    (right.id.to_string(), join.right_keys),
                ];
                Some(PolarsFrameOp {
                    reads,
                    columns: join.columns,
                })
            }
            _ => None,
        }
    }

    // The keys and result columns of `left.join(right, ...)` in polars, when the arguments
    // are literal.
    pub(super) fn polars_join(
        left_cols: &[String],
        right_cols: &[String],
        call: &ast::ExprCall,
    ) -> Option<PolarsJoin> {
        let key =
            |name: &str| keyword(call, name).map(|k| ast_extract::literal_key_columns(&k.value));
        let (left_keys, right_keys) = match (key("on"), key("left_on"), key("right_on")) {
            (Some(on), None, None) => (on.clone()?, on?),
            (None, Some(l), Some(r)) => (l?, r?),
            _ => return None,
        };
        let how = match keyword(call, "how").map(|k| &k.value) {
            None => "inner".to_string(),
            Some(Expr::StringLiteral(s)) => s.value.to_str().to_string(),
            Some(_) => return None,
        };
        // `coalesce=` and `validate=` change which key columns survive.
        if keyword(call, "coalesce").is_some() || call.arguments.args.len() > 1 {
            return None;
        }
        let columns = match how.as_str() {
            "semi" | "anti" => left_cols.to_vec(),
            "inner" | "left" => {
                let suffix = match keyword(call, "suffix").map(|k| &k.value) {
                    None => "_right".to_string(),
                    Some(Expr::StringLiteral(s)) => s.value.to_str().to_string(),
                    Some(_) => return None,
                };
                let mut columns = left_cols.to_vec();
                for name in right_cols.iter().filter(|c| !right_keys.contains(c)) {
                    let out = if left_cols.contains(name) {
                        format!("{name}{suffix}")
                    } else {
                        name.clone()
                    };
                    if columns.contains(&out) {
                        return None;
                    }
                    columns.push(out);
                }
                columns
            }
            _ => return None,
        };
        Some(PolarsJoin {
            left_keys,
            right_keys,
            columns,
        })
    }

    pub(super) fn apply_polars_frame_op(
        &mut self,
        assign: &ast::StmtAssign,
        op: PolarsFrameOp,
        line: usize,
        col: usize,
        errors: &mut Vec<LintError>,
    ) {
        for (frame, names) in &op.reads {
            let Some((schema, defined_line)) = self.variables.get(frame).cloned() else {
                continue;
            };
            if self.schema_is_unresolved(&schema) {
                continue;
            }
            let available = self.schemas.get(&schema).cloned().unwrap_or_default();
            for name in names.iter().filter(|n| !available.contains(n)) {
                let mut message = format!(
                    "Column '{}' does not exist in {}",
                    name,
                    self.schema_display(&schema, defined_line)
                );
                if let Some(suggestion) = find_best_match(name, &available) {
                    message.push_str(&format!(" (did you mean '{suggestion}'?)"));
                }
                errors.push(LintError {
                    line,
                    col,
                    code: CODE_UNKNOWN_COLUMN.to_string(),
                    message,
                    severity: "error".to_string(),
                });
            }
        }
        let targets = Self::assign_target_names(assign);
        let Some(var_hint) = targets.first().cloned() else {
            return;
        };
        let base = op.reads.first().map(|(f, _)| f.clone()).unwrap_or_default();
        let base_schema = self.variables.get(&base).map(|(s, _)| s.clone());
        let schema_name = self.count_mutation_leg(
            base_schema.as_deref(),
            Some(op.columns),
            &var_hint,
            line,
            col,
        );
        for name in &targets {
            self.variables
                .insert(name.clone(), (schema_name.clone(), line));
        }
    }
}

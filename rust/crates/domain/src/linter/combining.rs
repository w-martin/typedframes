use super::*;

impl Linter {
    pub(super) fn assign_added_columns(
        &mut self,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        attr: &ruff_python_ast::ExprAttribute,
        func_name: &str,
        call: &ruff_python_ast::ExprCall,
    ) {
        if let Expr::Name(recv) = &*attr.value {
            let recv_str = recv.id.as_str();
            let (base_info, new_cols, target_names, var_hint) =
                self.added_columns_of(assign, func_name, recv_str, call);
            if base_info.is_some() {
                let base_resolved = base_info
                    .as_deref()
                    .is_some_and(|s| !self.schema_is_unresolved(s));
                let schema_name = match new_cols {
                    None if base_resolved => {
                        let known = self
                            .schemas
                            .get(base_info.as_deref().unwrap_or_default())
                            .cloned()
                            .unwrap_or_default();
                        self.record_leg_event(
                            &var_hint,
                            current_line,
                            current_col,
                            "unresolved",
                            format!(
                                "`.{func_name}()` adds columns whose names cannot be read ({}), so only the columns `{recv_str}` had are known",
                                unnamed_argument_hint(call)
                            ),
                        );
                        self.count_open_leg(
                            base_info.as_deref(),
                            known,
                            &var_hint,
                            current_line,
                            current_col,
                        )
                    }
                    new_cols => self.count_mutation_leg(
                        base_info.as_deref(),
                        new_cols,
                        &var_hint,
                        current_line,
                        current_col,
                    ),
                };
                for name in &target_names {
                    self.variables
                        .insert(name.clone(), (schema_name.clone(), current_line));
                }
            }
        }
    }

    pub(super) fn register_merge_result(
        &mut self,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        is_merge_or_concat: bool,
        merge_schema: Option<Vec<String>>,
        merge_call: Option<&ruff_python_ast::ExprCall>,
    ) {
        if is_merge_or_concat {
            if let Some(operands) = merge_schema {
                // The merged/concatenated result is a new DataFrame origin in
                // its own right -- every input was already a tracked frame --
                // so it's always recognized (dataframes_total). Whether it
                // counts as TYPED depends on the inputs: an unresolved input
                // keeps the result unresolved (see below), which is why the
                // count_typed_dataframe call waits until `unresolved_schemas`
                // has been updated.
                self.register_merged_frame(assign, current_line, current_col, merge_call, operands);
            }
        }
    }

    fn register_merged_frame(
        &mut self,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        merge_call: Option<&ruff_python_ast::ExprCall>,
        operands: Vec<String>,
    ) {
        self.dataframes_total += 1;

        // A concat holds every column of every operand. A merge holds
        // both sides' columns with pandas' suffixes on the overlap, so
        // the bare name of a collided column is gone.
        let column_sets: Vec<Vec<String>> = operands
            .iter()
            .map(|s| self.schemas.get(s).cloned().unwrap_or_default())
            .collect();
        let mut combined_cols = Vec::new();
        let mut suffixes_unknown = false;
        match merge_call {
            Some(call) => match merged_columns(&column_sets[0], &column_sets[1], call) {
                Merged::Columns(columns) => combined_cols = columns,
                Merged::Unknown => suffixes_unknown = true,
            },
            None => combined_cols.extend(column_sets.into_iter().flatten()),
        }
        combined_cols.sort();
        combined_cols.dedup();

        let var_hint = assign_var_hint(assign, "df");
        // Named schemas keep the name of what they combine, which is
        // what the diagnostic then shows.
        let joined_name = operands.join("_");
        let combined_schema_name = if operands.iter().all(|s| !s.starts_with("__inferred_"))
            && self
                .schemas
                .get(&joined_name)
                .is_none_or(|existing| *existing == combined_cols)
        {
            self.schemas.insert(joined_name.clone(), combined_cols);
            joined_name
        } else {
            self.make_inferred_schema(combined_cols, &var_hint, current_line)
        };
        // An unresolved schema (see `register_feast_dataframe`,
        // `resolve_open_or_named_schema`) stays unresolved after a
        // merge/concat: its column list was already known to be
        // incomplete before the join, and combining it with another
        // frame's columns doesn't make it any more complete.
        if suffixes_unknown || operands.iter().any(|s| self.schema_is_unresolved(s)) {
            self.unresolved_schemas.insert(combined_schema_name.clone());
        }
        self.count_typed_dataframe(&combined_schema_name, &var_hint, current_line, current_col);
        for target in &assign.targets {
            if let Expr::Name(target_name) = target {
                self.variables.insert(
                    target_name.id.to_string(),
                    (combined_schema_name.clone(), current_line),
                );
            }
        }
    }

    pub(super) fn record_concat(
        &mut self,
        current_line: usize,
        is_merge_or_concat: &mut bool,
        merge_schema: &mut Option<Vec<String>>,
        call: &ruff_python_ast::ExprCall,
    ) {
        if let Some(schemas) = self.concat_schemas(call, current_line) {
            *is_merge_or_concat = true;
            *merge_schema = Some(schemas);
        }
    }

    fn added_columns_of(
        &mut self,
        assign: &ruff_python_ast::StmtAssign,
        func_name: &str,
        recv_str: &str,
        call: &ruff_python_ast::ExprCall,
    ) -> (Option<String>, Option<Vec<String>>, Vec<String>, String) {
        let base_info = self.variables.get(recv_str).map(|(s, _)| s.clone());
        // `.assign()` keeps the receiver's columns, so an untracked
        // receiver must not yield a frame of only the new ones.
        if base_info.is_none() {
            for target in &assign.targets {
                if let Expr::Name(n) = target {
                    self.variables.remove(n.id.as_str());
                }
            }
        }
        let mut base_cols: Vec<String> = base_info
            .as_ref()
            .and_then(|s| self.schemas.get(s).cloned())
            .unwrap_or_default();
        // An expression whose output name can't be read leaves the
        // result's columns unknown rather than guessed.
        let added = if func_name == "with_columns" {
            self.with_columns_outputs(call)
        } else {
            ast_extract::assign_output_names(call)
        };
        // `pl.col("x").cast(..)` on a closed frame without `x` raises
        // (reported below); the fixed code won't output `x`, so
        // keeping it would hide later typos of the same name.
        let base_closed = base_info
            .as_deref()
            .is_some_and(|s| !self.schema_is_unresolved(s));
        let added = match added {
            Some(names) if func_name == "with_columns" && base_closed => {
                let unaliased = ast_extract::with_columns_unaliased_outputs(call);
                Some(
                    names
                        .into_iter()
                        .filter(|n| base_cols.contains(n) || !unaliased.contains(n))
                        .collect::<Vec<_>>(),
                )
            }
            other => other,
        };
        let new_cols = added.map(|names| {
            for name in names {
                if !base_cols.contains(&name) {
                    base_cols.push(name);
                }
            }
            base_cols
        });
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
        let var_hint = target_names
            .first()
            .cloned()
            .unwrap_or_else(|| recv_str.to_string());
        (base_info, new_cols, target_names, var_hint)
    }
}

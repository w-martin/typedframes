use super::*;

impl Linter {
    // Column set for a Feast `features=["view:feature", ...]` list: each element must
    // be a `"view:feature"` string literal (the ref format Feast documents), and the
    // resulting column is the part after the colon — or, when `full_feature_names=True`,
    // `"view__feature"` (double underscore, per Feast's own docstring). Bails entirely
    // (`None`) on: no `features=` keyword at all (so a plain method-name match alone
    // never dispatches — see `FEAST_RETRIEVAL_METHODS`'s call sites), a non-literal
    // list element (a `Name`, a comprehension, a `FeatureService` member projection
    // like `fv[["conv_rate"]]`), an element without exactly one `:`, or a non-literal
    // `full_feature_names`. Never a partial column set for the same reason as
    // `extract_select_args`: a silently-dropped element would understate the real
    // projection.
    //
    // Not the complete output on its own — Feast's real output also includes
    // entity_df's own columns; see `resolve_feast_entity_columns`.
    pub(super) fn extract_feast_feature_columns(
        &self,
        call: &ast::ExprCall,
    ) -> Option<Vec<String>> {
        let features_list = call
            .arguments
            .keywords
            .iter()
            .find(|k| k.arg.as_ref().map(|s| s.as_str()) == Some("features"))
            .map(|k| &k.value)?;

        let full_feature_names = match call
            .arguments
            .keywords
            .iter()
            .find(|k| k.arg.as_ref().map(|s| s.as_str()) == Some("full_feature_names"))
        {
            Some(kw) => match &kw.value {
                Expr::BooleanLiteral(b) => b.value,
                _ => return None,
            },
            None => false,
        };

        ast_extract::feast_columns_from_list_expr(features_list, full_feature_names)
    }

    // Feast's output is entity_df's columns unioned with the requested features
    // (Feast docs: retrieval keeps the entity rows and adds the joined feature
    // values). Resolves entity_df to a concrete schema if we can; `None` if it
    // isn't a plain `Name`, or is itself unresolved -- no partial credit.
    pub(super) fn resolve_feast_entity_columns(&self, call: &ast::ExprCall) -> Option<Vec<String>> {
        let entity_expr = call
            .arguments
            .keywords
            .iter()
            .find(|k| k.arg.as_ref().map(|s| s.as_str()) == Some("entity_df"))
            .map(|k| &k.value)?;
        let Expr::Name(entity_name) = entity_expr else {
            return None;
        };
        let (schema_name, _) = self.variables.get(entity_name.id.as_str())?;
        if self.unresolved_schemas.contains(schema_name) {
            return None;
        }
        self.schemas.get(schema_name).cloned()
    }

    // Shared by `extract_feast_feature_columns` above (the `features=` keyword found
    // inline at its own call) and `resolve_param_governed_call_sites` (a literal list
    // found at a call site elsewhere, substituted in for a parameter that governed the
    // original call) — both need the exact same "view:feature" splitting and
    // `full_feature_names` formatting applied to a literal list expression.
    pub(super) fn find_param_governed_feast_template(
        &self,
        func_def: &ast::StmtFunctionDef,
    ) -> Option<ParamGovernedTemplate> {
        let param_names: Vec<&str> = func_def
            .parameters
            .posonlyargs
            .iter()
            .chain(func_def.parameters.args.iter())
            .map(|p| p.parameter.name.id.as_str())
            .collect();

        let found = self.find_governed_feast_load(func_def, param_names);

        let (
            param_name,
            param_index,
            full_feature_names,
            target_var,
            found_at,
            governing_line,
            governing_col,
        ) = found?;

        let mut accesses = Vec::new();
        for stmt in &func_def.body[found_at + 1..] {
            self.collect_subscript_accesses(stmt, &target_var, &mut accesses);
        }
        // No `accesses.is_empty()` bailout here — a function with nothing to validate
        // internally (e.g. `print(df)`, no subscript at all) is STILL param-governed
        // and resolvable via call-site tracing; the intra-function untracked-dataframe
        // warning is just as stale for it as for one with recorded accesses. Bailing
        // out here would leave that warning uncorrected purely because there happens
        // to be nothing to check, which is a different question from whether the call
        // is resolvable at all.

        Some(ParamGovernedTemplate {
            param_name,
            param_index,
            full_feature_names,
            accesses,
            governing_line,
            governing_col,
        })
    }

    fn find_governed_feast_load(
        &self,
        func_def: &ruff_python_ast::StmtFunctionDef,
        param_names: Vec<&str>,
    ) -> Option<(String, usize, bool, String, usize, usize, usize)> {
        let mut found: Option<(String, usize, bool, String, usize, usize, usize)> = None;
        // (param_name, param_index, full_feature_names, target_var, stmt_index, governing_line, governing_col)

        for (i, stmt) in func_def.body.iter().enumerate() {
            let Stmt::Assign(assign) = stmt else {
                continue;
            };
            let Some(Expr::Name(target_name)) = assign.targets.first() else {
                continue;
            };
            let Expr::Call(outer_call) = &*assign.value else {
                continue;
            };
            let Expr::Attribute(outer_attr) = &*outer_call.func else {
                continue;
            };
            if outer_attr.attr.as_str() != "to_df" {
                continue;
            }
            let Expr::Call(inner_call) = &*outer_attr.value else {
                continue;
            };
            let Expr::Attribute(inner_attr) = &*inner_call.func else {
                continue;
            };
            if !FEAST_RETRIEVAL_METHODS.contains(&inner_attr.attr.as_str()) {
                continue;
            }
            let Some(features_kw) = inner_call
                .arguments
                .keywords
                .iter()
                .find(|k| k.arg.as_ref().map(|s| s.as_str()) == Some("features"))
            else {
                continue;
            };
            let Expr::Name(features_name) = &features_kw.value else {
                continue;
            };
            let Some(param_index) = param_names
                .iter()
                .position(|p| *p == features_name.id.as_str())
            else {
                continue;
            };
            let full_feature_names = match inner_call
                .arguments
                .keywords
                .iter()
                .find(|k| k.arg.as_ref().map(|s| s.as_str()) == Some("full_feature_names"))
            {
                Some(kw) => match &kw.value {
                    Expr::BooleanLiteral(b) => b.value,
                    _ => false,
                },
                None => false,
            };
            let (governing_line, governing_col) = self.source_location(assign.range().start());
            found = Some((
                features_name.id.to_string(),
                param_index,
                full_feature_names,
                target_name.id.to_string(),
                i,
                governing_line,
                governing_col,
            ));
            break;
        }
        found
    }

    pub(super) fn assign_feast_retrieval(
        &mut self,
        errors: &mut Vec<LintError>,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        attr: &ruff_python_ast::ExprAttribute,
    ) {
        let feast_cols: Option<FeastColumns> = match &*attr.value {
            Expr::Name(recv) => self
                .retrieval_jobs
                .get(recv.id.as_str())
                .cloned()
                .map(|(cols, entity_cols)| (Some(cols), entity_cols)),
            Expr::Call(inner_call) => match &*inner_call.func {
                Expr::Attribute(inner_attr)
                    if FEAST_RETRIEVAL_METHODS.contains(&inner_attr.attr.as_str()) =>
                {
                    Some((
                        self.extract_feast_feature_columns(inner_call),
                        self.resolve_feast_entity_columns(inner_call),
                    ))
                }
                _ => None,
            },
            _ => None,
        };
        if let Some((cols, entity_cols)) = feast_cols {
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
            self.register_feast_dataframe(
                (cols, entity_cols),
                &target_names,
                var_name,
                current_line,
                current_col,
                errors,
            );
        }
    }
}

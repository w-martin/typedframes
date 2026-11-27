use super::*;

impl Linter {
    pub(super) fn record_returned_call(&mut self, func_def: &ruff_python_ast::StmtFunctionDef) {
        if let Some(call) = only_returned_call(&func_def.body) {
            self.record_returned_method_call(func_def, call);
        }
    }

    fn record_returned_method_call(
        &mut self,
        func_def: &ruff_python_ast::StmtFunctionDef,
        call: &ruff_python_ast::ExprCall,
    ) {
        if let Expr::Attribute(attr) = &*call.func {
            self.record_returned_module_call(func_def, call, attr);
        }
    }

    fn record_returned_module_call(
        &mut self,
        func_def: &ruff_python_ast::StmtFunctionDef,
        call: &ruff_python_ast::ExprCall,
        attr: &ruff_python_ast::ExprAttribute,
    ) {
        if let Expr::Name(module) = &*attr.value {
            self.record_returned_load(func_def, call, attr, module);
        }
    }

    fn record_returned_load(
        &mut self,
        func_def: &ruff_python_ast::StmtFunctionDef,
        call: &ruff_python_ast::ExprCall,
        attr: &ruff_python_ast::ExprAttribute,
        module: &ruff_python_ast::ExprName,
    ) {
        if LOAD_MODULES.contains(&module.id.as_str())
            && LOAD_FUNCTIONS.contains(&attr.attr.as_str())
        {
            // A column list that isn't literal says nothing about
            // the columns, whatever else the call passes.
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
            if let (Some(cols), true) = (
                self.extract_load_columns(attr.attr.as_str(), call).0,
                literal_lists,
            ) {
                let line = self.source_location(call.range().start()).0;
                let schema = self.make_inferred_schema(cols, func_def.name.as_str(), line);
                self.functions.insert(func_def.name.to_string(), schema);
            }
        }
    }

    pub(super) fn infer_column_contract(
        &mut self,
        func_def: &ruff_python_ast::StmtFunctionDef,
        fn_def_line: usize,
    ) {
        let mut candidate_params = func_def
            .parameters
            .posonlyargs
            .iter()
            .chain(func_def.parameters.args.iter());
        let first_param = match candidate_params.next() {
            Some(p) if matches!(p.parameter.name.id.as_str(), "self" | "cls") => {
                candidate_params.next()
            }
            other => other,
        };
        if let Some(first_param) = first_param {
            let param_name = first_param.parameter.name.id.as_str();
            let mut tainted = std::collections::HashSet::new();
            tainted.insert(param_name.to_string());
            let mut required = Vec::new();
            let mut delegates = Vec::new();
            for body_stmt in &func_def.body {
                contract::analyze_stmt_for_contract(
                    body_stmt,
                    &mut tainted,
                    &mut required,
                    &mut delegates,
                );
            }

            // Capture the annotation's schema *name* unconditionally, even when
            // it can't be resolved to columns yet (the schema may live in a
            // third file, not yet visible to this single-file Linter pass).
            // resolve_transitive_requires resolves it project-wide once every
            // file's schemas are known — see param_schema_name on IndexFunction.
            let annotation_schema_name: Option<String> = first_param
                .parameter
                .annotation
                .as_ref()
                .and_then(|a| ast_extract::extract_schema_from_annotation(a))
                .map(|s| s.to_string());
            if let Some(name) = &annotation_schema_name {
                self.param_schema_names
                    .insert(func_def.name.to_string(), (name.clone(), fn_def_line));
            }
            let annotated_cols = annotation_schema_name
                .as_deref()
                .and_then(|schema_name| self.schemas.get(schema_name).cloned());
            if let Some(cols) = annotated_cols {
                required = cols;
            }

            let created = contract::created_names(&func_def.body);
            required.retain(|column| !created.contains(column));
            required.sort();
            required.dedup();
            delegates.sort();
            delegates.dedup();
            if !required.is_empty() || !delegates.is_empty() {
                if call_contracts::is_straight_line(&func_def.body) {
                    self.straight_line_functions
                        .insert(func_def.name.to_string());
                }
                self.requires
                    .insert(func_def.name.to_string(), (required.clone(), fn_def_line));
                if !delegates.is_empty() {
                    self.delegates.insert(func_def.name.to_string(), delegates);
                }
            }
        }
    }
}

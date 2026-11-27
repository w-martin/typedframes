use super::*;

impl Linter {
    pub(super) fn visit_function_def(
        &mut self,
        errors: &mut Vec<LintError>,
        func_def: &ruff_python_ast::StmtFunctionDef,
    ) {
        let (fn_def_line, _) = self.source_location(func_def.range().start());
        self.visit_function_body(errors, func_def, fn_def_line);
        // Infer a column *contract* for the function's first parameter: every
        // column subscripted directly off that parameter or a variable derived
        // from it (`param["col"]`, or `x["col"]` where `x = param` / `x = f(param)`),
        // plus every function this one delegates its own parameter to. The
        // parameter itself carries no schema here — the caller's variable does —
        // so this is only a requirement to check against later, not something we
        // can validate inside the function body itself. Delegate targets are
        // resolved to a *transitive* union only at the project-index level (see
        // resolve_transitive_requires) — this pass records only the direct
        // requirements and the raw delegate names.
        //
        // If the first parameter carries a schema annotation, its declared column
        // list replaces the heuristic body-scan as the *direct* requirement — the
        // annotation is an explicit, complete contract, whereas the body-scan is
        // only a best-effort proxy used when no such contract exists. Delegates are
        // still collected and unioned in as usual: columns a forwarded call needs
        // only make the contract stricter, never wrong.
        // A leading `self`/`cls` is never the DataFrame being passed around --
        // it's the instance/class the method is bound to -- so it must not be
        // the parameter tainted below. Skipping past it (rather than, say,
        // requiring an explicit annotation to disambiguate) is what makes this
        // heuristic work at all for the extremely common case of a DataFrame
        // passed as an instance method's second parameter.
        self.infer_column_contract(func_def, fn_def_line);
    }

    fn visit_function_body(
        &mut self,
        errors: &mut Vec<LintError>,
        func_def: &ruff_python_ast::StmtFunctionDef,
        fn_def_line: usize,
    ) {
        self.all_function_names.insert(func_def.name.to_string());

        // The body gets its own copy of the variable map: enclosing frames stay
        // readable, parameters shadow them, and nothing bound inside leaks back
        // out (bar names declared `global`) -- see the restore below.
        let outer_variables = self.variables.clone();
        let parameters = &func_def.parameters;
        // A nested function sees its enclosing function's instances, except where a
        // parameter shadows one.
        let mut receiver_classes_init = self.receiver_classes.clone();
        for name in parameters
            .posonlyargs
            .iter()
            .chain(parameters.args.iter())
            .chain(parameters.kwonlyargs.iter())
            .map(|p| p.parameter.name.id.as_str())
            .chain(parameters.vararg.iter().map(|p| p.name.id.as_str()))
            .chain(parameters.kwarg.iter().map(|p| p.name.id.as_str()))
        {
            receiver_classes_init.remove(name);
        }
        let annotated_params: Vec<(String, String)> = parameters
            .posonlyargs
            .iter()
            .chain(parameters.args.iter())
            .chain(parameters.kwonlyargs.iter())
            .filter_map(|p| {
                let class = match &**p.parameter.annotation.as_ref()? {
                    Expr::Name(name) => name.id.to_string(),
                    Expr::StringLiteral(text) => text.value.to_str().to_string(),
                    _ => return None,
                };
                self.class_methods
                    .contains_key(&class)
                    .then(|| (p.parameter.name.id.to_string(), class))
            })
            .collect();
        receiver_classes_init.extend(annotated_params);
        let outer_receiver_classes =
            std::mem::replace(&mut self.receiver_classes, receiver_classes_init);
        let mut record_params_init = self.record_params.clone();
        for p in parameters
            .posonlyargs
            .iter()
            .chain(parameters.args.iter())
            .chain(parameters.kwonlyargs.iter())
        {
            let name = p.parameter.name.id.to_string();
            match p.parameter.annotation.as_deref() {
                Some(Expr::Name(class)) if self.record_fields.contains_key(class.id.as_str()) => {
                    record_params_init.insert(name, class.id.to_string());
                }
                _ => {
                    record_params_init.remove(&name);
                }
            }
        }
        let outer_record_params = self.visit_function_scope(
            errors,
            func_def,
            fn_def_line,
            parameters,
            record_params_init,
        );
        self.receiver_classes = outer_receiver_classes;
        self.record_params = outer_record_params;
        let mut function_variables = std::mem::replace(&mut self.variables, outer_variables);
        for name in global_names(&func_def.body) {
            match function_variables.remove(&name) {
                Some(binding) => self.variables.insert(name, binding),
                None => self.variables.remove(&name),
            };
        }
    }

    fn visit_function_scope(
        &mut self,
        errors: &mut Vec<LintError>,
        func_def: &ruff_python_ast::StmtFunctionDef,
        fn_def_line: usize,
        parameters: &ruff_python_ast::Parameters,
        record_params_init: HashMap<String, String>,
    ) -> HashMap<String, String> {
        let (outer_record_params, is_bare_frame_return) = self.visit_function_parameters(
            errors,
            func_def,
            fn_def_line,
            parameters,
            record_params_init,
        );

        let outer_open_frames = std::mem::take(&mut self.open_frames);
        let outer_open_reads = self.open_reads.take();
        for p in func_def
            .parameters
            .posonlyargs
            .iter()
            .chain(func_def.parameters.args.iter())
            .chain(func_def.parameters.kwonlyargs.iter())
        {
            let is_frame = p
                .parameter
                .annotation
                .as_deref()
                .is_some_and(ast_extract::extract_frame_parameter_type);
            if is_frame && !self.variables.contains_key(p.parameter.name.id.as_str()) {
                self.open_frames.insert(
                    p.parameter.name.id.to_string(),
                    OpenFrame {
                        depth: self.block_depth + 1,
                        ..OpenFrame::default()
                    },
                );
            }
        }
        for name in self
            .method_frames
            .get(&func_def.range().start())
            .cloned()
            .unwrap_or_default()
        {
            self.variables.remove(&name);
            self.open_frames.insert(
                name,
                OpenFrame {
                    depth: self.block_depth + 1,
                    ..OpenFrame::default()
                },
            );
        }
        self.visit_body(&func_def.body, errors);
        self.open_frames = outer_open_frames;
        self.open_reads.replace(outer_open_reads);
        // Detect a parameter feeding a Feast features= call whose result is
        // subscripted in this same body — see ParamGovernedTemplate's doc
        // comment and resolve_param_governed_call_sites. Whether the
        // untracked-dataframe warning register_feast_dataframe already pushed
        // for this exact statement should be retracted depends on whether any
        // ACTUAL call site anywhere in the project was ever traced back to it —
        // not just on this shape existing — so that decision is deferred to
        // check_file, informed by ProjectIndex.resolved_governed (see its doc
        // comment for why: a governed call site that fails to resolve now gets
        // its OWN untracked-dataframe diagnostic at the call site, so retracting
        // the callee's line is safe whenever some call site was seen; only a
        // function with no discoverable call site anywhere keeps this line as
        // its sole diagnostic, so retracting unconditionally would silently go
        // from "we tell you it's unknown" to "we tell you nothing at all").
        self.detect_governed_parameter(func_def);
        // Lowest-priority fallback: a bare `-> pd.DataFrame`/`-> pl.DataFrame`
        // return annotation with nothing more specific found above (no named
        // Schema, no body-inferred one either -- e.g. the function's real
        // source, like a `py.typed` package's, isn't available to infer from).
        // OPEN_FRAME_MARKER — see its doc comment — is what tells call-site
        // resolution to register an open/empty schema rather than treat this
        // as a real schema name.
        if is_bare_frame_return && !self.functions.contains_key(func_def.name.as_str()) {
            self.functions
                .insert(func_def.name.to_string(), OPEN_FRAME_MARKER.to_string());
        }
        outer_record_params
    }

    fn visit_function_parameters(
        &mut self,
        errors: &mut Vec<LintError>,
        func_def: &ruff_python_ast::StmtFunctionDef,
        fn_def_line: usize,
        parameters: &ruff_python_ast::Parameters,
        record_params_init: HashMap<String, String>,
    ) -> (HashMap<String, String>, bool) {
        let outer_record_params = std::mem::replace(&mut self.record_params, record_params_init);
        for name in parameters
            .posonlyargs
            .iter()
            .chain(parameters.args.iter())
            .chain(parameters.kwonlyargs.iter())
            .map(|p| p.parameter.name.id.as_str())
            .chain(parameters.vararg.iter().map(|p| p.name.id.as_str()))
            .chain(parameters.kwarg.iter().map(|p| p.name.id.as_str()))
        {
            self.variables.remove(name);
        }

        // Track return type annotations like -> DataFrame[Schema]. A bare
        // `-> pd.DataFrame` / `-> pl.DataFrame` (no attached Schema) is a
        // LOWER-PRIORITY fallback -- see `is_bare_frame_return` below, checked
        // once body-inference has had its chance, not here: an explicit named
        // Schema always wins, but so does a body-inferred one (e.g. `load_orders`
        // resolving real SQL-derived columns via `return df`) over a bare
        // annotation that merely says "some DataFrame, columns unknown".
        let mut is_bare_frame_return = false;
        if let Some(returns) = &func_def.returns {
            if let Some(schema_name) = ast_extract::extract_schema_from_annotation(returns) {
                self.functions
                    .insert(func_def.name.to_string(), schema_name.to_string());
            } else if ast_extract::extract_bare_dataframe_type(returns) {
                is_bare_frame_return = true;
            }
        }

        // Schema-annotated parameters (`def f(df: DataFrame[Schema])`,
        // `Annotated[pd.DataFrame, Schema]`, or a quoted equivalent) get tracked
        // in self.variables exactly like a local `df: DataFrame[Schema] = ...`
        // assignment would — so accesses inside the body are validated against
        // the declared schema, the same as anywhere else in the file, rather
        // than left unchecked just because the binding came from a parameter.
        for p in func_def
            .parameters
            .posonlyargs
            .iter()
            .chain(func_def.parameters.args.iter())
            .chain(func_def.parameters.kwonlyargs.iter())
        {
            self.note_parameter_annotation(errors, fn_def_line, p);
        }
        (outer_record_params, is_bare_frame_return)
    }

    fn note_parameter_annotation(
        &mut self,
        errors: &mut Vec<LintError>,
        fn_def_line: usize,
        p: &ruff_python_ast::ParameterWithDefault,
    ) {
        if let Some(annotation) = &p.parameter.annotation {
            if let Some(schema_name) = ast_extract::extract_schema_from_annotation(annotation) {
                if !self.schemas.contains_key(schema_name) {
                    let (line, col) = self.source_location(p.range().start());
                    errors.push(LintError {
                        line,
                        col,
                        code: CODE_UNRESOLVED_SCHEMA.to_string(),
                        message: format!(
                            "Schema `{schema_name}` annotating parameter `{}` is not defined here or in any import the checker could follow, so no column on it is checked (a file checked on its own cannot follow every import; check the whole project)",
                            p.parameter.name.id
                        ),
                        severity: "warning".to_string(),
                    });
                }
                self.variables.insert(
                    p.parameter.name.id.to_string(),
                    (schema_name.to_string(), fn_def_line),
                );
            }
        }
    }

    fn detect_governed_parameter(&mut self, func_def: &ruff_python_ast::StmtFunctionDef) {
        if let Some(template) = self.find_param_governed_feast_template(func_def) {
            self.param_governed_templates
                .insert(func_def.name.to_string(), template);
        }
        // If no annotation-based mapping, infer from `return <var>`.
        // After visiting the body, self.variables holds the schema of every
        // local variable; look up the returned one and register the function.
        if !self.functions.contains_key(func_def.name.as_str()) {
            self.record_returned_variable_schema(func_def);
        }
        // `return pd.DataFrame({...})` / `return pd.read_csv(..., usecols=[...])`:
        // the frame is never named, so the `return <var>` inference above can't
        // see it, but its columns are as known as if it had been.
        self.record_function_return(func_def);
    }

    fn record_returned_variable_schema(&mut self, func_def: &ruff_python_ast::StmtFunctionDef) {
        if let Some(var_name) = find_returned_var(&func_def.body) {
            if let Some((schema_name, defined_line)) = self.variables.get(&var_name) {
                let schema_name = schema_name.clone();
                let defined_line = *defined_line;
                if self.schemas.contains_key(&schema_name) {
                    // A bare `return <var>` hands this function's own
                    // internal origin straight to its caller -- e.g.
                    // `def b(): c = get_dataframe(); return c`. Ownership
                    // of the count SHOULD move to every call site that
                    // resolves through this function (`a = b()`, handled in
                    // the Expr::Name/self.<attr> call-resolution sites,
                    // which mark `functions_called` and count it there)
                    // rather than double-counting both here AND at each
                    // call site -- one `a = b()` is one DataFrame, not two.
                    //
                    // But whether any such call site actually EXISTS isn't
                    // knowable yet at this point in the walk (a call site
                    // may appear later in this same file, or nowhere at
                    // all if this function is only reached via a same-file
                    // transitive chain -- `return other_func()`, not
                    // `return <var>` -- which isn't traced today). Removing
                    // the count now, unconditionally, would risk losing it
                    // with nowhere to land if nothing ever confirms a call.
                    // So this only records a CANDIDATE; the actual removal
                    // is deferred to the end of this file's walk (see
                    // `check_parsed_module`), applied only for candidates
                    // `functions_called` confirms were actually consulted
                    // by a resolving call site anywhere in THIS file.
                    //
                    // Scoped to this file only: a function called solely
                    // from ANOTHER file (defined in loaders.py, called from
                    // pipeline.py) is never confirmed here, so its origin
                    // stays counted in the defining file's own tally for
                    // now -- cross-file confirmation needs project-wide
                    // knowledge of call sites, which is a separate,
                    // larger piece of work, not addressed by this patch.
                    //
                    // Also only ever matches a count that exactly matches
                    // `var_name`'s CURRENT binding line: a pass-through hop
                    // between the origin/mutation and this `return` (e.g.
                    // `c = c.head(10)` in between) moves `defined_line`
                    // without leaving a matching typed/untyped_sites entry
                    // there, so the reversal silently doesn't fire and that
                    // narrower case still double-counts -- a known gap, not
                    // addressed here.
                    self.pending_passthrough_reversals.insert(
                        func_def.name.to_string(),
                        PendingPassthroughReversal {
                            var: var_name.clone(),
                            line: defined_line,
                        },
                    );
                    self.functions
                        .insert(func_def.name.to_string(), schema_name);
                }
            }
        }
    }

    fn record_function_return(&mut self, func_def: &ruff_python_ast::StmtFunctionDef) {
        if !self.functions.contains_key(func_def.name.as_str()) {
            self.record_returned_call(func_def);
        }
        // `return df.rename(...)` / `return df[["a"]]` / `return cursor.fetch_pandas_all()`:
        // a returned expression whose columns follow from tracked frames and
        // literal edits, which neither inference above handles.
        if !self.functions.contains_key(func_def.name.as_str()) {
            if let Some(expr) = only_returned_expr(&func_def.body) {
                if let Some(cols) = self.returned_expr_columns(expr) {
                    let line = self.source_location(expr.range().start()).0;
                    let schema = self.make_inferred_schema(cols, func_def.name.as_str(), line);
                    self.functions.insert(func_def.name.to_string(), schema);
                }
            }
        }
        // `return a, b` of frames whose columns are known here, for `x, y = f()`.
        self.tuple_functions.remove(func_def.name.as_str());
        if let Some(Expr::Tuple(tuple)) = only_returned_expr(&func_def.body) {
            let line = self.source_location(tuple.range().start()).0;
            let schemas: Vec<Option<String>> = tuple
                .elts
                .iter()
                .map(|e| {
                    let cols = self.returned_expr_columns(e)?;
                    Some(self.make_inferred_schema(cols, func_def.name.as_str(), line))
                })
                .collect();
            if schemas.iter().any(Option::is_some) {
                self.tuple_functions
                    .insert(func_def.name.to_string(), schemas);
            }
        }
    }
}

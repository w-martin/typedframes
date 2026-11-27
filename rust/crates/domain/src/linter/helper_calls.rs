use super::*;

impl Linter {
    // `class` followed by its ancestors defined in this file, nearest first.
    pub(super) fn class_chain(&self, class: &str) -> Vec<String> {
        class_chain(&self.class_bases, class)
    }

    // The return schema of `self.<method>()` in the class being visited: its own method if
    // already visited, else an inherited one.
    pub(super) fn resolve_self_method(&self, method: &str) -> Option<String> {
        if let Some(schema) = self.current_class_functions.get(method) {
            return Some(schema.clone());
        }
        let class = self.current_class_name.as_ref()?;
        self.class_chain(class)
            .iter()
            .skip(1)
            .find_map(|ancestor| self.class_methods.get(ancestor)?.get(method).cloned())
    }

    // The return schema (or `OPEN_FRAME_MARKER`) of `class.method`, for a call through a
    // parameter annotated with that class. An interface's own method is usually a bare
    // `-> pd.DataFrame`; when it is, and every other class in the file that defines the
    // method returns one and the same column set, that is the answer.
    pub(super) fn resolve_method_on_class(&self, class: &str, method: &str) -> Option<String> {
        let chain = self.class_chain(class);
        let own = chain
            .iter()
            .find_map(|c| self.class_methods.get(c)?.get(method))?
            .clone();
        if own != OPEN_FRAME_MARKER {
            return Some(own);
        }
        let mut implementations = self
            .class_methods
            .iter()
            .filter(|(name, _)| !chain.contains(name))
            .filter_map(|(_, methods)| methods.get(method))
            .filter(|s| s.as_str() != OPEN_FRAME_MARKER);
        let Some(first) = implementations.next() else {
            return Some(own);
        };
        let columns = |schema: &String| {
            (!self.schema_is_unresolved(schema))
                .then(|| self.schemas.get(schema).cloned())
                .flatten()
        };
        let agreed = columns(first).is_some_and(|expected| {
            implementations.all(|other| columns(other).as_ref() == Some(&expected))
        });
        Some(if agreed { first.clone() } else { own })
    }

    // Binds every plain-name target of `assign` to the schema a function or method with
    // a recognized return annotation (or a body-inferred return) produces, counting the
    // result as one DataFrame.
    pub(super) fn bind_function_result(
        &mut self,
        assign: &ast::StmtAssign,
        marker_or_schema: &str,
        func_name: &str,
        line: usize,
        col: usize,
    ) {
        let schema_name = self.resolve_open_or_named_schema(marker_or_schema, func_name, line);
        // Confirms any pending pass-through reversal for this function -- see
        // `pending_passthrough_reversals`.
        self.functions_called.insert(func_name.to_string());
        self.dataframes_total += 1;
        let var_hint = assign_var_hint(assign, func_name);
        self.count_typed_dataframe(&schema_name, &var_hint, line, col);
        if marker_or_schema == OPEN_FRAME_MARKER {
            self.record_leg_event(
                &var_hint,
                line,
                col,
                "unresolved",
                format!("`{func_name}()` is annotated to return a bare DataFrame and its columns could not be derived from its argument"),
            );
        }
        for target in &assign.targets {
            if let Expr::Name(target_name) = target {
                self.variables
                    .insert(target_name.id.to_string(), (schema_name.clone(), line));
            }
        }
    }

    // A call `f(x)` to a helper whose summary is known, `x` a tracked frame.
    //
    // - Edits the helper makes to the caller's own object (`in_place`) move `x` to a new
    //   leg regardless of what the call's result is used for -- a bare statement
    //   (`normalize(x)`), an assignment whose target is `targets` (which may or may not
    //   include `x` itself), or the return value being discarded entirely (`msg =
    //   validate(x)`, where `validate` mutates `x` but returns a status string).
    // - When the helper returns its parameter, each of `targets` ALSO carries the
    //   returned frame: `x`'s schema with the helper's edits applied. With no edits the
    //   frame is the one `x` already counted; with edits it is a new leg. `targets` is
    //   left untouched when the helper doesn't return its parameter (`end_stale_legs`,
    //   run after every statement, untracks a target whose binding still doesn't
    //   otherwise account for the assignment).
    pub(super) fn apply_helper_call(
        &mut self,
        value: &Expr,
        targets: &[String],
        line: usize,
        col: usize,
    ) {
        let Some((arg, summary)) = self.helper_call(value) else {
            return;
        };
        let Some(original) = self.variables.get(&arg).cloned() else {
            return;
        };
        let what = describe_rhs(value);
        // Rebinding the argument's own name to the result makes an update to it moot.
        let arg_binding = if targets.contains(&arg) && summary.returned.is_some() {
            None
        } else {
            let binding = self.edited_binding(&original, &summary.in_place, &arg, &what, line, col);
            self.variables.insert(arg.clone(), binding.clone());
            Some(binding)
        };
        let Some(returned) = &summary.returned else {
            return;
        };
        let result = match arg_binding {
            Some(binding) if *returned == summary.in_place => binding,
            _ => {
                let var = targets.first().unwrap_or(&arg);
                self.edited_binding(&original, returned, var, &what, line, col)
            }
        };
        for target in targets {
            self.variables.insert(target.clone(), result.clone());
        }
    }

    // `binding` with `edits` applied: itself when there are none (or its columns are
    // already unknown), else a new counted leg -- unresolved if an edit is not understood.
    fn edited_binding(
        &mut self,
        binding: &(String, usize),
        edits: &[ColumnEdit],
        var: &str,
        what: &str,
        line: usize,
        col: usize,
    ) -> (String, usize) {
        if edits.is_empty() || self.schema_is_unresolved(&binding.0) {
            return binding.clone();
        }
        let base = self.schemas.get(&binding.0).cloned().unwrap_or_default();
        let columns = apply_edits(&base, edits);
        let resolved = columns.is_some();
        let schema = self.count_mutation_leg(Some(&binding.0), columns, var, line, col);
        if !resolved {
            self.record_leg_event(
                var,
                line,
                col,
                "unresolved",
                format!("{what} edits the columns in a way that is not modelled"),
            );
        }
        (schema, line)
    }

    // The forwarder call `unwrap_forwarder` left alone because its callable is not a plain name.
    pub(super) fn note_unfollowed_forwarder(&mut self, assign: &ast::StmtAssign) {
        let Expr::Call(call) = &*assign.value else {
            return;
        };
        let Expr::Name(forwarder) = &*call.func else {
            return;
        };
        let Some(&(callable_at, _)) = self.forwarders.get(forwarder.id.as_str()) else {
            return;
        };
        if call.arguments.args.get(callable_at).is_none() {
            return;
        }
        let (line, col) = self.source_location(assign.range().start());
        for target in Self::assign_target_names(assign) {
            self.record_leg_event(
                &target,
                line,
                col,
                "untracked",
                format!(
                    "`{}()` forwards a callable that is not a plain function name, so its result is not tracked",
                    forwarder.id
                ),
            );
        }
    }

    // `self.df: Annotated[pd.DataFrame, Missing] = ...`: an attribute is not a tracked name, so
    // nothing is checked on it and it is not counted, but a schema nothing defines is still said.
    pub(super) fn warn_unresolved_attribute_schema(
        &self,
        ann_assign: &ast::StmtAnnAssign,
        line: usize,
        col: usize,
        errors: &mut Vec<LintError>,
    ) {
        let Some(schema_name) = ast_extract::extract_schema_from_annotation(&ann_assign.annotation)
        else {
            return;
        };
        if self.schemas.contains_key(schema_name) {
            return;
        }
        errors.push(LintError {
            line,
            col,
            code: CODE_UNRESOLVED_SCHEMA.to_string(),
            message: format!(
                "Schema `{schema_name}` annotating an attribute is not defined here or in any import the checker could follow, so no column on it is checked (a file checked on its own cannot follow every import; check the whole project)"
            ),
            severity: "warning".to_string(),
        });
    }

    // `out = run("stage", fn, raw)` where `run` only calls `fn(*args)`: the statement the caller
    // meant, `out = fn(raw)`. Only when `fn` is a plain name; a lambda or an expression stays
    // as written, untracked.
    pub(super) fn unwrap_forwarder(&self, assign: &ast::StmtAssign) -> Option<ast::StmtAssign> {
        let Expr::Call(call) = &*assign.value else {
            return None;
        };
        let Expr::Name(forwarder) = &*call.func else {
            return None;
        };
        let &(callable_at, forwarded_from) = self.forwarders.get(forwarder.id.as_str())?;
        if !matches!(call.arguments.args.get(callable_at)?, Expr::Name(_)) {
            return None;
        }
        let mut unwrapped = assign.clone();
        let mut inner = call.clone();
        *inner.func = call.arguments.args[callable_at].clone();
        inner.arguments.args = call
            .arguments
            .args
            .get(forwarded_from..)
            .unwrap_or_default()
            .to_vec()
            .into_boxed_slice();
        inner.arguments.keywords = Default::default();
        unwrapped.value = Box::new(Expr::Call(inner));
        Some(unwrapped)
    }

    // A bare `-> pd.DataFrame` return says nothing about columns, but the callee's body may:
    // when it hands its first parameter back, the summary carries that frame's columns through.
    pub(super) fn summary_beats_open_marker(&self, marker: &str, value: &Expr) -> bool {
        marker == OPEN_FRAME_MARKER
            && self
                .helper_call(value)
                .is_some_and(|(_, summary)| summary.returned.is_some())
    }

    pub(super) fn assign_target_names(assign: &ast::StmtAssign) -> Vec<String> {
        assign
            .targets
            .iter()
            .filter_map(|t| match t {
                Expr::Name(n) => Some(n.id.to_string()),
                _ => None,
            })
            .collect()
    }

    // A call to a helper defined further down this file: the walk has not reached its
    // definition yet, so its return schema is not inferred yet -- not evidence the
    // callee is unknown.
    pub(super) fn calls_later_defined_function(&self, value: &Expr) -> bool {
        let mut expr = value;
        while let Expr::Await(awaited) = expr {
            expr = &awaited.value;
        }
        let Expr::Call(call) = expr else {
            return false;
        };
        let Some(name) = self.callee_name(call) else {
            return false;
        };
        self.file_function_names.contains(name) && !self.all_function_names.contains(name)
    }

    // `f` for `f(...)`, `m` for `self.m(...)` / `cls.m(...)`.
    fn callee_name<'c>(&self, call: &'c ast::ExprCall) -> Option<&'c str> {
        match &*call.func {
            Expr::Name(name) => Some(name.id.as_str()),
            Expr::Attribute(attr) => match &*attr.value {
                Expr::Name(receiver) if matches!(receiver.id.as_str(), "self" | "cls") => {
                    Some(attr.attr.as_str())
                }
                _ => None,
            },
            _ => None,
        }
    }

    // For `clean(df)` / `self._clean(df)` where the callee's summary is known: the tracked
    // variable it is handed, and that summary.
    pub(super) fn helper_call(&self, value: &Expr) -> Option<(String, HelperSummary)> {
        let mut expr = value;
        while let Expr::Await(awaited) = expr {
            expr = &awaited.value;
        }
        let Expr::Call(call) = expr else {
            return None;
        };
        let summary = match &*call.func {
            Expr::Name(name) => self.helper_summaries.get(&(None, name.id.to_string()))?,
            Expr::Attribute(attr) => match &*attr.value {
                Expr::Name(receiver) if matches!(receiver.id.as_str(), "self" | "cls") => {
                    let class = self.current_class_name.as_ref()?;
                    self.class_chain(class).into_iter().find_map(|c| {
                        self.helper_summaries.get(&(Some(c), attr.attr.to_string()))
                    })?
                }
                // `loader.add(df)` on an instance of a class defined in this file.
                Expr::Name(receiver)
                    if !self.variables.contains_key(receiver.id.as_str())
                        && self.receiver_classes.contains_key(receiver.id.as_str()) =>
                {
                    let class = self.receiver_classes.get(receiver.id.as_str())?;
                    self.class_chain(class).into_iter().find_map(|c| {
                        self.helper_summaries.get(&(Some(c), attr.attr.to_string()))
                    })?
                }
                // `Ratios().add(df)`.
                Expr::Call(constructed) => {
                    let Expr::Name(class) = &*constructed.func else {
                        return None;
                    };
                    if !self.class_bases.contains_key(class.id.as_str()) {
                        return None;
                    }
                    self.class_chain(class.id.as_ref())
                        .into_iter()
                        .find_map(|c| {
                            self.helper_summaries.get(&(Some(c), attr.attr.to_string()))
                        })?
                }
                // `module.f(df)` for a module imported with `import module`.
                Expr::Name(module) => self.helper_summaries.get(&(
                    Some(format!("module:{}", module.id.as_str())),
                    attr.attr.to_string(),
                ))?,
                _ => return None,
            },
            _ => return None,
        };
        match call.arguments.args.first()? {
            Expr::Name(arg) if self.variables.contains_key(arg.id.as_str()) => {
                Some((arg.id.to_string(), summary.clone()))
            }
            _ => None,
        }
    }
}

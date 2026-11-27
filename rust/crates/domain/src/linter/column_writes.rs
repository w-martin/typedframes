use super::*;

impl Linter {
    // Validate any `pl.col("name")` / `col("name")` references in a call's arguments
    // against the schema of a tracked receiver variable.
    pub(super) fn validate_pl_col_args_on_receiver(
        &self,
        recv_name: &str,
        call: &ast::ExprCall,
        line: usize,
        col: usize,
        errors: &mut Vec<LintError>,
    ) {
        let Some((schema_name, defined_line)) =
            self.variables.get(recv_name).map(|(s, l)| (s.clone(), *l))
        else {
            return;
        };
        if self.schema_is_unresolved(&schema_name) {
            return;
        }
        let Some(columns) = self.schemas.get(&schema_name).cloned() else {
            return;
        };
        let col_names: Vec<String> = call
            .arguments
            .args
            .iter()
            .flat_map(ast_extract::collect_pl_col_names)
            .chain(
                call.arguments
                    .keywords
                    .iter()
                    .flat_map(|kw| ast_extract::collect_pl_col_names(&kw.value)),
            )
            .collect();
        for col_name in col_names {
            if !columns.contains(&col_name) {
                let schema_display = self.schema_display(&schema_name, defined_line);
                let mut message =
                    format!("Column '{}' does not exist in {}", col_name, schema_display);
                if let Some(suggestion) = find_best_match(&col_name, &columns) {
                    message.push_str(&format!(" (did you mean '{}'?)", suggestion));
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
    }

    // A write of one or more columns into a tracked frame (`df["c"] = ...`,
    // `df[["a", "b"]] = ...`, `df.loc[rows, "c"] = ...`). A column the schema does not
    // hold is reported, then the frame moves to a new leg that includes it; against an
    // unresolved frame the write is unverifiable and the leg stays unresolved.
    pub(super) fn record_column_writes(
        &mut self,
        recv: &str,
        col_names: &[String],
        line: usize,
        col: usize,
        errors: &mut Vec<LintError>,
    ) {
        let Some((schema_name, defined_line)) = self.variables.get(recv).cloned() else {
            return;
        };
        if self.schema_is_unresolved(&schema_name) {
            // A write reads nothing, so it is not itself unverifiable; the first read of
            // the frame says so.
            // An edit attempted against an unresolved base can't be trusted to have
            // produced any particular column set either -- see `count_mutation_leg`.
            let new_schema = self.count_mutation_leg(Some(&schema_name), None, recv, line, col);
            self.variables.insert(recv.to_string(), (new_schema, line));
            return;
        }
        let missing: Vec<&String> = col_names
            .iter()
            .filter(|c| !self.schema_has_column(&schema_name, c))
            .collect();
        if missing.is_empty() {
            return;
        }
        let mut new_cols = self.schemas.get(&schema_name).cloned().unwrap_or_default();
        // Only a declared schema is a contract a write can break; a column set the
        // checker inferred just gains the column.
        let declared = !schema_name.starts_with("__inferred_");
        let schema_display = self.schema_display(&schema_name, defined_line);
        for col_name in missing {
            if declared {
                errors.push(LintError {
                    line,
                    col,
                    code: CODE_UNKNOWN_COLUMN.to_string(),
                    message: format!(
                        "Column '{}' does not exist in {} (mutation tracking)",
                        col_name, schema_display
                    ),
                    severity: "error".to_string(),
                });
            }
            if !new_cols.contains(col_name) {
                new_cols.push(col_name.clone());
            }
        }
        // A new leg rather than an edit of the old schema object, which another name
        // may share.
        let new_schema =
            self.count_mutation_leg(Some(&schema_name), Some(new_cols), recv, line, col);
        self.variables.insert(recv.to_string(), (new_schema, line));
    }

    // A write under a computed name to a frame whose columns are inferred: it may add any
    // column, so the frame is open afterwards, keeping what it certainly has.
    pub(super) fn record_dynamic_column_write(&mut self, recv: &str, line: usize, col: usize) {
        let Some((schema_name, _)) = self.variables.get(recv).cloned() else {
            return;
        };
        if self.schema_is_unresolved(&schema_name) || !schema_name.starts_with("__inferred_") {
            return;
        }
        let known = self.schemas.get(&schema_name).cloned().unwrap_or_default();
        let new_schema = self.count_open_leg(Some(&schema_name), known, recv, line, col);
        self.variables.insert(recv.to_string(), (new_schema, line));
        self.record_leg_event(
            recv,
            line,
            col,
            "unresolved",
            "a column is written under a name computed at runtime, so the columns added are unknown"
                .to_string(),
        );
    }

    // The names `key` takes over the iterations of the enclosing `for` loops, when each loop
    // runs over a fixed list and every iteration yields a string.
    pub(super) fn loop_computed_names(&self, key: &Expr) -> Option<Vec<String>> {
        if self.loop_bindings.is_empty() {
            return None;
        }
        let environments = self.loop_environments()?;
        let mut names = Vec::new();
        for env in &environments {
            let resolver = static_eval::LoopNames { bound: env };
            match static_eval::Evaluator::new(&resolver).eval(key)? {
                static_eval::Value::Str(name) if !names.contains(&name) => names.push(name),
                static_eval::Value::Str(_) => {}
                _ => return None,
            }
        }
        (!names.is_empty()).then_some(names)
    }

    // Every combination of values the enclosing literal loops bind (one empty one outside a
    // loop), or `None` when there are too many to enumerate.
    fn loop_environments(&self) -> Option<Vec<HashMap<String, static_eval::Value>>> {
        let mut environments: Vec<HashMap<String, static_eval::Value>> = vec![HashMap::new()];
        for binding in &self.loop_bindings {
            let mut next = Vec::new();
            for env in &environments {
                for row in &binding.rows {
                    let mut env = env.clone();
                    env.extend(binding.names.iter().cloned().zip(row.iter().cloned()));
                    next.push(env);
                }
            }
            if next.len() > MAX_COMBINATIONS {
                return None;
            }
            environments = next;
        }
        Some(environments)
    }

    // The output names of a polars `with_columns`/`agg` call. Beyond the literal forms, a list
    // argument, a comprehension over a literal list, and `alias(...)`/`pl.col(...)` names
    // computed from loop variables are evaluated for every value the loops bind.
    pub(super) fn with_columns_outputs(&self, call: &ast::ExprCall) -> Option<Vec<String>> {
        if let Some(names) = ast_extract::with_columns_output_names(call) {
            return Some(names);
        }
        let mut names = ast_extract::assign_output_names(call)?;
        let envs = self.loop_environments()?;
        let push = |found: Vec<String>, names: &mut Vec<String>| {
            for name in found {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        };
        for arg in &call.arguments.args {
            self.with_columns_output_of(&mut names, &envs, push, arg)?;
        }
        Some(names)
    }

    fn with_columns_output_of(
        &self,
        names: &mut Vec<String>,
        envs: &Vec<HashMap<String, static_eval::Value>>,
        push: impl Fn(Vec<String>, &mut Vec<String>),
        arg: &Expr,
    ) -> Option<()> {
        let _: () = match arg {
            Expr::List(ast::ExprList { elts, .. }) | Expr::Tuple(ast::ExprTuple { elts, .. }) => {
                for elt in elts {
                    push(self.pl_outputs_over(elt, envs)?, names);
                }
            }
            Expr::ListComp(comp) => {
                let [generator] = comp.generators.as_slice() else {
                    return None;
                };
                let Expr::Name(target) = &generator.target else {
                    return None;
                };
                if !generator.ifs.is_empty() || generator.is_async {
                    return None;
                }
                for env in envs {
                    let resolver = static_eval::LoopNames { bound: env };
                    let items = static_eval::Evaluator::new(&resolver)
                        .eval_iterable(&generator.iter)
                        .and_then(|v| static_eval::Evaluator::iterate(&v))?;
                    let inner: Vec<_> = items
                        .into_iter()
                        .map(|item| {
                            let mut env = env.clone();
                            env.insert(target.id.to_string(), item);
                            env
                        })
                        .collect();
                    push(self.pl_outputs_over(&comp.elt, &inner)?, names);
                }
            }
            _ => push(self.pl_outputs_over(arg, envs)?, names),
        };
        Some(())
    }

    fn pl_outputs_over(
        &self,
        expr: &Expr,
        envs: &[HashMap<String, static_eval::Value>],
    ) -> Option<Vec<String>> {
        if let Some(name) = ast_extract::pl_expr_output_name(expr) {
            return Some(vec![name]);
        }
        let mut names = Vec::new();
        for env in envs {
            let name = Self::pl_output_in_env(expr, env)?;
            if !names.contains(&name) {
                names.push(name);
            }
        }
        (!names.is_empty()).then_some(names)
    }

    // `pl_expr_output_name` with names bound by a loop: `alias("g_" + g)`, `pl.col(c)`.
    fn pl_output_in_env(expr: &Expr, env: &HashMap<String, static_eval::Value>) -> Option<String> {
        let eval_str = |e: &Expr| {
            let resolver = static_eval::LoopNames { bound: env };
            match static_eval::Evaluator::new(&resolver).eval(e)? {
                // A `^...$` pattern selects many columns.
                static_eval::Value::Str(s) if !(s.starts_with('^') && s.ends_with('$')) => Some(s),
                _ => None,
            }
        };
        if let Some(name) = ast_extract::pl_expr_output_name(expr) {
            return Some(name);
        }
        let Expr::Call(call) = expr else {
            return None;
        };
        let Expr::Attribute(attr) = &*call.func else {
            return None;
        };
        match (attr.attr.as_str(), &*attr.value, &*call.arguments.args) {
            ("alias", _, [arg]) => eval_str(arg),
            ("col", Expr::Name(m), [arg]) if matches!(m.id.as_str(), "pl" | "polars") => {
                eval_str(arg)
            }
            (_, Expr::Call(inner), _) => {
                if matches!(&*inner.func, Expr::Attribute(a) if a.attr.as_str() == "alias") {
                    return None;
                }
                Self::pl_output_in_env(&attr.value, env)
            }
            // A namespace (`.str`, `.dt`) keeps the name; these rename.
            (_, Expr::Attribute(ns), _) if !matches!(ns.attr.as_str(), "name" | "struct") => {
                Self::pl_output_in_env(&ns.value, env)
            }
            _ => None,
        }
    }

    pub(super) fn enter_loop(&mut self, for_stmt: &ast::StmtFor) -> bool {
        let resolver = static_eval::LoopNames {
            bound: &HashMap::new(),
        };
        let Some(items) = static_eval::Evaluator::new(&resolver)
            .eval_iterable(&for_stmt.iter)
            .and_then(|v| static_eval::Evaluator::iterate(&v))
        else {
            return false;
        };
        let names: Option<Vec<String>> = match &*for_stmt.target {
            Expr::Name(n) => Some(vec![n.id.to_string()]),
            Expr::Tuple(t) => t
                .elts
                .iter()
                .map(|e| match e {
                    Expr::Name(n) => Some(n.id.to_string()),
                    _ => None,
                })
                .collect(),
            _ => None,
        };
        let Some(names) = names else {
            return false;
        };
        let single = names.len() == 1 && matches!(&*for_stmt.target, Expr::Name(_));
        let rows: Option<Vec<Vec<static_eval::Value>>> = items
            .into_iter()
            .map(|item| match (single, item) {
                (true, item) => Some(vec![item]),
                (false, static_eval::Value::Tuple(v) | static_eval::Value::List(v))
                    if v.len() == names.len() =>
                {
                    Some(v)
                }
                _ => None,
            })
            .collect();
        match rows {
            Some(rows) => {
                self.loop_bindings.push(LoopBinding { names, rows });
                true
            }
            None => false,
        }
    }

    // Whether `receiver` is a polars frame, so a method both libraries have is read the polars way.
    pub(super) fn is_polars_receiver(&self, receiver: &Expr) -> bool {
        self.polars_only_file || crate::polars_names::is_polars_expr(receiver, &self.polars_names)
    }

    // Whether a call runs in place: a literal `inplace=True`, or a name that the whole
    // file binds exactly once, to a literal. Anything else is assumed not to.
    pub(super) fn runs_in_place(&self, call: &ast::ExprCall) -> bool {
        match keyword(call, "inplace").map(|k| &k.value) {
            Some(Expr::BooleanLiteral(b)) => b.value,
            Some(Expr::Name(name)) => self
                .bool_var_candidates
                .get(name.id.as_str())
                .copied()
                .unwrap_or(false),
            _ => false,
        }
    }
}

use super::*;

impl Linter {
    // Recursively collects every `<target_var>["literal"]` subscript access reachable
    // from `stmt`, matching the same nesting scope as `analyze_stmt_for_contract`
    // (Return/Expr/Assign/AnnAssign/If/For/While/With) for consistency with the rest of
    // this checker's "conservative rather than exhaustive" heuristics.
    pub(super) fn collect_subscript_accesses(
        &self,
        stmt: &Stmt,
        target_var: &str,
        out: &mut Vec<ParamGovernedAccess>,
    ) {
        match stmt {
            Stmt::Return(ret) => {
                if let Some(value) = &ret.value {
                    self.collect_subscripts_in_expr(value, target_var, out);
                }
            }
            Stmt::Expr(expr_stmt) => {
                self.collect_subscripts_in_expr(&expr_stmt.value, target_var, out)
            }
            Stmt::Assign(assign) => self.collect_subscripts_in_expr(&assign.value, target_var, out),
            Stmt::AnnAssign(ann) => {
                if let Some(value) = &ann.value {
                    self.collect_subscripts_in_expr(value, target_var, out);
                }
            }
            Stmt::If(if_stmt) => {
                for s in &if_stmt.body {
                    self.collect_subscript_accesses(s, target_var, out);
                }
                for clause in &if_stmt.elif_else_clauses {
                    for s in &clause.body {
                        self.collect_subscript_accesses(s, target_var, out);
                    }
                }
            }
            Stmt::For(for_stmt) => {
                for s in &for_stmt.body {
                    self.collect_subscript_accesses(s, target_var, out);
                }
            }
            Stmt::While(while_stmt) => {
                for s in &while_stmt.body {
                    self.collect_subscript_accesses(s, target_var, out);
                }
            }
            Stmt::With(with_stmt) => {
                for s in &with_stmt.body {
                    self.collect_subscript_accesses(s, target_var, out);
                }
            }
            _ => {}
        }
    }

    // Recursively finds every `<target_var>["literal"]` subscript reachable inside
    // `expr` — a real access is very rarely the statement's own top-level expression
    // shape (`print(df["col"])` is a Call whose argument is the subscript, not a bare
    // subscript statement), so this has to actually walk into the common wrapping
    // shapes rather than checking `expr` alone the way a first pass might assume.
    fn collect_subscripts_in_expr(
        &self,
        expr: &Expr,
        target_var: &str,
        out: &mut Vec<ParamGovernedAccess>,
    ) {
        if let Expr::Subscript(sub) = expr {
            self.collect_subscript(expr, target_var, out, sub);
            // Deliberately not recursing into `sub.value`/`sub.slice` further here —
            // a subscript's own base is either the target (handled above) or
            // something else entirely, and nested chained subscripts on unrelated
            // bases aren't this checker's concern.
            return;
        }
        match expr {
            Expr::Call(call) => {
                self.collect_subscripts_in_expr(&call.func, target_var, out);
                for arg in &call.arguments.args {
                    self.collect_subscripts_in_expr(arg, target_var, out);
                }
                for kw in &call.arguments.keywords {
                    self.collect_subscripts_in_expr(&kw.value, target_var, out);
                }
            }
            Expr::Attribute(attr) => {
                self.collect_subscripts_in_expr(&attr.value, target_var, out);
            }
            Expr::BinOp(bin) => {
                self.collect_subscripts_in_expr(&bin.left, target_var, out);
                self.collect_subscripts_in_expr(&bin.right, target_var, out);
            }
            Expr::UnaryOp(unary) => {
                self.collect_subscripts_in_expr(&unary.operand, target_var, out);
            }
            Expr::BoolOp(bool_op) => {
                for v in &bool_op.values {
                    self.collect_subscripts_in_expr(v, target_var, out);
                }
            }
            Expr::Compare(cmp) => {
                self.collect_subscripts_in_expr(&cmp.left, target_var, out);
                for c in &cmp.comparators {
                    self.collect_subscripts_in_expr(c, target_var, out);
                }
            }
            Expr::Tuple(t) => {
                for el in &t.elts {
                    self.collect_subscripts_in_expr(el, target_var, out);
                }
            }
            Expr::List(l) => {
                for el in &l.elts {
                    self.collect_subscripts_in_expr(el, target_var, out);
                }
            }
            Expr::Starred(s) => {
                self.collect_subscripts_in_expr(&s.value, target_var, out);
            }
            _ => {}
        }
    }

    fn collect_subscript(
        &self,
        expr: &Expr,
        target_var: &str,
        out: &mut Vec<ParamGovernedAccess>,
        sub: &ruff_python_ast::ExprSubscript,
    ) {
        if let Expr::Name(recv) = &*sub.value {
            if recv.id.as_str() == target_var {
                if let Some(col) = ast_extract::extract_string_literal(&sub.slice) {
                    let (line, col_num) = self.source_location(expr.range().start());
                    out.push(ParamGovernedAccess {
                        line,
                        col: col_num,
                        column: col.to_string(),
                    });
                }
            }
        }
    }

    // Record a DataFrame origin that was counted in `dataframes_total` but whose
    // columns could not be resolved. Called at every counting site that does not
    // bump `dataframes_typed`, so `untyped_sites.len()` always equals
    // `dataframes_total - dataframes_typed`.
    pub(super) fn record_untyped_site(
        &mut self,
        var_hint: &str,
        current_line: usize,
        current_col: usize,
    ) {
        self.untyped_sites.push(UntypedSite {
            line: current_line,
            col: current_col,
            var: var_hint.to_string(),
            phrase: String::new(),
        });
    }

    // Bumps `dataframes_typed` for a concrete schema, or records an `untyped_sites`
    // entry for an unresolved one (caller owns `dataframes_total += 1` separately).
    // Callers must insert into `unresolved_schemas` before calling this.
    pub(super) fn count_typed_dataframe(
        &mut self,
        schema_name: &str,
        var_hint: &str,
        current_line: usize,
        current_col: usize,
    ) {
        if self.unresolved_schemas.contains(schema_name) {
            self.record_untyped_site(var_hint, current_line, current_col);
        } else {
            self.dataframes_typed += 1;
            self.typed_sites.push(TypedSite {
                line: current_line,
                col: current_col,
                var: var_hint.to_string(),
                schema: self.schema_display(schema_name, current_line),
            });
        }
    }

    // Count a schema-changing mutation (rename/drop/assign) as a new DataFrame origin
    // in its own right, distinct from whatever origin its receiver came from -- the
    // "a modification starts a new leg" half of the journey/leg model (see
    // `pending_passthrough_reversals`'s doc comment for the pass-through half).
    // `base_schema` is the receiver's CURRENT schema name, if tracked at all;
    // `derived_cols` is the newly computed column set, when the specific edit could be
    // resolved from a known base. Three outcomes:
    //
    // - The base itself is unresolved (`schema_is_unresolved`): the edit can't be
    //   trusted to have produced any particular column set from an unknown starting
    //   point, so the result propagates as unresolved too, rather than fabricating a
    //   schema derived from an empty/placeholder base.
    // - The base is resolved and `derived_cols` is `Some`: a genuine new schema,
    //   counted and typed.
    // - The base is resolved but `derived_cols` is `None` (the specific edit --
    //   e.g. a dynamic `.drop(some_list)` -- couldn't be extracted): still a KNOWN
    //   schema-changing method, so still counted as an origin, just untracked --
    //   rather than silently falling through as an unlabelled pass-through of the
    //   OLD schema, which risks validating later access against columns that are
    //   now wrong.
    pub(super) fn count_mutation_leg(
        &mut self,
        base_schema: Option<&str>,
        derived_cols: Option<Vec<String>>,
        var_hint: &str,
        current_line: usize,
        current_col: usize,
    ) -> String {
        self.dataframes_total += 1;
        let base_unresolved = base_schema.is_some_and(|s| self.schema_is_unresolved(s));
        let resolved_cols = if base_unresolved { None } else { derived_cols };
        let unresolved = resolved_cols.is_none();
        let schema_name =
            self.make_inferred_schema(resolved_cols.unwrap_or_default(), var_hint, current_line);
        if unresolved {
            self.unresolved_schemas.insert(schema_name.clone());
            if let Some(base) = base_schema.filter(|_| base_unresolved) {
                let root = self.unresolved_root_of(base);
                self.unresolved_root.insert(schema_name.clone(), root);
            }
        }
        self.count_typed_dataframe(&schema_name, var_hint, current_line, current_col);
        schema_name
    }

    // An edit that adds columns the checker cannot name: the frame is open from here on, but
    // the columns it certainly has stay known.
    pub(super) fn count_open_leg(
        &mut self,
        base_schema: Option<&str>,
        known: Vec<String>,
        var_hint: &str,
        current_line: usize,
        current_col: usize,
    ) -> String {
        let schema_name =
            self.count_mutation_leg(base_schema, None, var_hint, current_line, current_col);
        let mut known = known;
        for column in base_schema
            .and_then(|b| self.partial_columns.get(b))
            .cloned()
            .unwrap_or_default()
        {
            if !known.contains(&column) {
                known.push(column);
            }
        }
        self.partial_columns.insert(schema_name.clone(), known);
        schema_name
    }

    pub(super) fn is_known_on_open_leg(&self, schema_name: &str, column: &str) -> bool {
        self.partial_columns
            .get(schema_name)
            .is_some_and(|known| known.iter().any(|c| c == column))
    }

    fn unresolved_root_of(&self, schema_name: &str) -> String {
        self.unresolved_root
            .get(schema_name)
            .cloned()
            .unwrap_or_else(|| schema_name.to_string())
    }

    // Whether this is the first `unverifiable-column` for the frame `schema_name` belongs
    // to; later ones add nothing the first did not say.
    pub(super) fn first_unverifiable_report(&self, schema_name: &str) -> bool {
        self.reported_unverifiable
            .borrow_mut()
            .insert(self.unresolved_root_of(schema_name))
    }

    pub(super) fn unverifiable_message(&self, col_name: &str, schema_name: &str) -> String {
        let root = self.unresolved_root_of(schema_name);
        let origin = inferred_schema_line(&root).map_or_else(
            || "the frame".to_string(),
            |line| format!("the frame from line {line}"),
        );
        format!(
            "Column '{col_name}' cannot be verified: {origin} has no known column list \
             (the checker could not derive it there); fix: give the frame its columns where it is \
             created (`usecols=`/`columns=`, or a schema annotation; `typedframes suggest` proposes \
             it) and every column use on it is checked. Reported once per frame."
        )
    }

    // Called at the end of the `x = call(...)` origin dispatch in the Assign handler,
    // after every recognized pattern (pd.read_csv/read_sql, connectorx, Feast,
    // SQL_PRODUCING_METHODS+SQL_FINALIZE_METHODS, Schema.from_pandas, a same-repo
    // function with a known return schema, ...) has had its chance to match. If none
    // did (`dataframes_total` is unchanged from before the dispatch) and the assigned
    // variable is later used somewhere in this file the way a DataFrame is used, per
    // `dataframe_shaped_usage`, records the call's receiver (attribute call) or callee
    // name (bare call) into `unresolved_dataframe_shaped_calls` — see that field's
    // docs for why this is silent bookkeeping only, never a diagnostic.
    pub(super) fn maybe_record_unresolved_dataframe_shaped_call(
        &mut self,
        assign: &ast::StmtAssign,
        call: &ast::ExprCall,
        dataframes_total_before: usize,
    ) {
        if self.dataframes_total != dataframes_total_before {
            return;
        }
        if assign.targets.len() != 1 {
            return;
        }
        let Expr::Name(target) = &assign.targets[0] else {
            return;
        };
        if !self.dataframe_shaped_usage.contains(target.id.as_str()) {
            return;
        }
        let Some((name, is_attribute_call, callee)) = self.unresolved_callee(&call.func) else {
            return;
        };
        let (line, col) = self.source_location(target.range().start());
        self.unresolved_dataframe_shaped_calls
            .push(UnresolvedDataframeShapedCall {
                name,
                is_attribute_call,
                line,
                col,
                var: target.id.to_string(),
                phrase: String::new(),
                callee,
            });
    }

    // The name to look a callee up by, whether it is called as an attribute, and what kind of
    // callee it is -- for a call this checker could not resolve to a frame.
    fn unresolved_callee(&self, func: &Expr) -> Option<(String, bool, &'static str)> {
        match func {
            Expr::Attribute(attr) => match &*attr.value {
                Expr::Name(base) => Some((
                    base.id.to_string(),
                    true,
                    "a method called on a local, parameter or imported module",
                )),
                // self.<attr>.<method>(...), where <attr>'s class (from
                // current_self_attrs) didn't resolve via class_methods -- not a
                // same-file first-party class with a known return type, so fall
                // back to the same external-package candidate discovery a plain
                // `module.func()` call already gets, using the receiver's
                // pre-resolved SelfAttrOrigin (see its doc comment for why
                // resolve_name/is_attribute_call is exactly what index.rs's
                // existing imports/module_aliases resolution needs).
                Expr::Attribute(recv_attr) => {
                    let Expr::Name(recv_base) = &*recv_attr.value else {
                        return None;
                    };
                    if recv_base.id.as_str() != "self" {
                        return None;
                    }
                    let origin = self.current_self_attrs.get(recv_attr.attr.as_str())?;
                    Some((
                        origin.resolve_name.clone(),
                        origin.is_attribute_call,
                        "a method called on an attribute of `self`",
                    ))
                }
                _ => None,
            },
            Expr::Name(func_name) => {
                Some((func_name.id.to_string(), false, "a plain function call"))
            }
            _ => None,
        }
    }
}

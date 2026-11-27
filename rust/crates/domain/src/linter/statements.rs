use super::*;

impl Linter {
    // Walk a statement node, updating linter state and collecting diagnostics.
    //
    // ClassDef      — detect BaseSchema subclasses; collect inherited + declared columns.
    // FunctionDef   — record annotated return types for cross-assignment schema tracking.
    // Assign        — track load calls, method-chain results (drop/rename/select/…),
    //                 DataFrame[Schema](...) instantiation, and merge/concat.
    // AnnAssign     — handle `df: Annotated[pd.DataFrame, S]` and quoted annotations.
    // Expr          — delegate column-access checks to visit_expr.
    // Delete        — handle `del df["col"]` in-place mutations.
    // If/For/While/With/Try — recurse into every nested body (see visit_body).
    pub(super) fn visit_stmt_inner(&mut self, stmt: &Stmt, errors: &mut Vec<LintError>) {
        match stmt {
            Stmt::ClassDef(class_def) => {
                self.visit_class_def(errors, class_def);
            }
            Stmt::FunctionDef(func_def) => {
                self.visit_function_def(errors, func_def);
            }
            Stmt::Assign(assign) => {
                self.visit_assign(errors, assign);
            }
            Stmt::AnnAssign(ann_assign) => {
                self.visit_annotated_assign(errors, ann_assign);
            }
            Stmt::Expr(expr_stmt) => {
                // A helper that edits the frame it is handed, called for its side effect.
                self.visit_expression_statement(errors, expr_stmt);
            }
            Stmt::Assert(assert) => {
                self.visit_expr(&assert.test, errors);
                if let Some(msg) = &assert.msg {
                    self.visit_expr(msg, errors);
                }
            }
            Stmt::AugAssign(aug) => {
                self.visit_expr(&aug.target, errors);
                self.visit_expr(&aug.value, errors);
            }
            Stmt::Delete(delete) => {
                self.visit_delete(errors, delete);
            }
            // `return <expr>` was never dispatched to visit_expr at all — a completely
            // separate gap from the BinOp/keyword-arg recursion fixed in visit_expr itself.
            // Any column access whose only appearance is in a return statement (extremely
            // common for small helper functions, e.g. `return df["a"] + df["bad"]`) was
            // silently invisible to validation.
            Stmt::Return(ret) => {
                if let Some(value) = &ret.value {
                    self.visit_expr(value, errors);
                }
            }
            // If/For/While/With/Try — a DataFrame origin or column access reached only
            // through control flow is exactly as real as one at the top level of a
            // function body; a linear top-to-bottom walk that skips these bodies
            // entirely (as this match used to, via the `_ => {}` fallback) misses
            // virtually every SQL/connection call in practice, since those are almost
            // always wrapped in a `with <connection>:` and/or `try`/`except`.
            Stmt::If(if_stmt) => {
                self.visit_if(errors, if_stmt);
            }
            Stmt::For(for_stmt) => {
                self.visit_for(errors, for_stmt);
            }
            Stmt::While(while_stmt) => {
                self.visit_expr(&while_stmt.test, errors);
                self.visit_body(&while_stmt.body, errors);
                self.visit_body(&while_stmt.orelse, errors);
            }
            Stmt::With(with_stmt) => {
                for item in &with_stmt.items {
                    self.visit_expr(&item.context_expr, errors);
                }
                self.visit_body(&with_stmt.body, errors);
            }
            Stmt::Try(try_stmt) => {
                self.visit_try(errors, try_stmt);
            }
            _ => {}
        }
    }

    fn visit_assign(&mut self, errors: &mut Vec<LintError>, assign: &ruff_python_ast::StmtAssign) {
        let (current_line, current_col) = self.visit_assign_targets(errors, assign);

        if let Expr::Call(call) = &*assign.value {
            let dataframes_total_before_call_dispatch = self.note_select_statement(assign);

            let mut merged = MergeOutcome::default();
            match &*call.func {
                Expr::Attribute(attr) => {
                    self.assign_attribute_call(
                        errors,
                        assign,
                        (current_line, current_col),
                        &mut merged,
                        attr,
                        call,
                    );
                }
                Expr::Name(name) if name.id.as_str() == "concat" => {
                    self.record_concat(
                        current_line,
                        &mut merged.is_merge_or_concat,
                        &mut merged.schema,
                        call,
                    );
                }
                _ => {}
            }

            self.register_merge_result(
                assign,
                current_line,
                current_col,
                merged.is_merge_or_concat,
                merged.schema,
                merged.call,
            );

            // Support for DataFrame[Schema](...) instantiation
            self.assign_schema_constructor(
                errors,
                assign,
                current_line,
                current_col,
                dataframes_total_before_call_dispatch,
                call,
            );
        }
        self.track_assignment_targets(errors, assign);
    }

    fn assign_attribute_call<'a>(
        &mut self,
        errors: &mut Vec<LintError>,
        assign: &'a ruff_python_ast::StmtAssign,
        (current_line, current_col): (usize, usize),
        merged: &mut MergeOutcome<'a>,
        attr: &'a ruff_python_ast::ExprAttribute,
        call: &'a ruff_python_ast::ExprCall,
    ) {
        let func_name = attr.attr.as_str();

        // Handle df = self.<attr>.<method>(...) where <attr>'s
        // class (known from __init__, see current_self_attrs) has
        // a <method> with a recognized return annotation (see
        // class_methods). None of the other receiver-shape checks
        // in this match arm (merge, chained SQL-producing calls,
        // etc.) apply to this shape -- the receiver here is
        // `self.<attr>`, never a tracked DataFrame variable or a
        // bare module/name -- so this is safe to check
        // unconditionally rather than needing an early return.
        self.assign_self_method_result(assign, current_line, current_col, attr, func_name);

        let at = (current_line, current_col);
        if !self.assign_combining_call(errors, assign, at, merged, attr, call) {
            self.assign_frame_method(errors, assign, at, attr, call);
        }
        // Validate pl.col() / col() references for any method call on a tracked variable.
        self.validate_pl_col_arguments(errors, current_line, current_col, attr, call);
    }

    // `merge`/`concat`, a load (`pd.read_csv(..)`, `Schema.from_pandas(..)`) or a column-selecting
    // `filter`; `false` for any other call.
    fn assign_combining_call<'a>(
        &mut self,
        errors: &mut Vec<LintError>,
        assign: &'a ruff_python_ast::StmtAssign,
        (current_line, current_col): (usize, usize),
        merged: &mut MergeOutcome<'a>,
        attr: &'a ruff_python_ast::ExprAttribute,
        call: &'a ruff_python_ast::ExprCall,
    ) -> bool {
        let func_name = attr.attr.as_str();
        if matches!(func_name, "merge" | "merge_asof" | "merge_ordered") {
            let operands: Vec<&Expr> = match &*attr.value {
                Expr::Name(module) if !self.variables.contains_key(module.id.as_str()) => {
                    call.arguments.args.iter().take(2).collect()
                }
                receiver => std::iter::once(receiver)
                    .chain(call.arguments.args.first())
                    .collect(),
            };
            if let [left, right] = operands.as_slice() {
                if let (Some(left), Some(right)) = (
                    self.operand_schema(left, current_line),
                    self.operand_schema(right, current_line),
                ) {
                    merged.is_merge_or_concat = true;
                    merged.schema = Some(vec![left, right]);
                    merged.call = Some(call);
                }
            }
        } else if func_name == "concat" {
            if let Some(schemas) = self.concat_schemas(call, current_line) {
                merged.is_merge_or_concat = true;
                merged.schema = Some(schemas);
            }
        } else if func_name == "from_pandas"
            || func_name == "from_polars"
            || LOAD_FUNCTIONS.contains(&func_name)
        {
            // Schema.from_pandas(df) / Schema.from_polars(df).
            if let Expr::Name(class_name) = &*attr.value {
                let class_str = class_name.id.as_str();
                if self.schemas.contains_key(class_str) {
                    // Schema.from_pandas(df) style
                    self.assign_schema_from_frame(assign, current_line, current_col, class_str);
                } else if LOAD_MODULES.contains(&class_str) && LOAD_FUNCTIONS.contains(&func_name) {
                    // pd.read_csv() / pl.scan_parquet() / pd.read_sql() etc.
                    self.assign_load(errors, assign, current_line, current_col, func_name, call);
                } else if CONNECTORX_MODULES.contains(&class_str) && func_name == "read_sql" {
                    // connectorx.read_sql(conn_uri, sql) — SQL is
                    // the second positional argument, the reverse of
                    // pandas' convention, so this can't reuse
                    // extract_load_columns/extract_sql_literal.
                    self.assign_connectorx_read(errors, assign, current_line, current_col, call);
                }
            }
        } else if func_name == "filter" && ast_extract::filter_is_column_selecting(call) {
            // pandas' `.filter(items=/like=/regex=)` selects
            // COLUMNS (or index labels, depending on axis=), unlike
            // the positional-mask/callable shape the
            // ROW_PASSTHROUGH_METHODS branch below handles for this
            // same method name -- checked first since the two
            // shapes are otherwise indistinguishable by name alone.
            if let Expr::Name(recv) = &*attr.value {
                let recv_str = recv.id.as_str();
                self.assign_column_filter(
                    errors,
                    assign,
                    current_line,
                    current_col,
                    recv_str,
                    call,
                );
            }
        } else {
            return false;
        }
        true
    }

    // A method called on a frame: what it does to the frame's columns.
    fn assign_frame_method(
        &mut self,
        errors: &mut Vec<LintError>,
        assign: &ruff_python_ast::StmtAssign,
        (current_line, current_col): (usize, usize),
        attr: &ruff_python_ast::ExprAttribute,
        call: &ruff_python_ast::ExprCall,
    ) {
        let func_name = attr.attr.as_str();
        if ROW_PASSTHROUGH_METHODS.contains(&func_name) {
            // Row-preserving ops: propagate base schema unchanged
            self.assign_row_preserving(assign, current_line, current_col, attr, func_name);
        } else if func_name == "select" {
            self.assign_select(errors, assign, current_line, current_col, attr, call);
        } else if FEAST_RETRIEVAL_METHODS.contains(&func_name) {
            // job = store.get_historical_features(entity_df=..., features=[...])
            // — the split form's first half. Not yet a DataFrame (a
            // RetrievalJob/OnlineResponse), so tracked in the
            // separate `retrieval_jobs` map rather than
            // `self.variables` — otherwise `job["x"]` before
            // `.to_df()` would be (wrongly) validated as a column
            // access. See `register_feast_dataframe` for where this
            // actually becomes a tracked DataFrame.
            self.assign_feast_job_frame(assign, call);
        } else if func_name == "to_df" {
            // Either half of Feast's two DataFrame-materializing
            // shapes: the split form's second half
            // (`df = job.to_df()`, `job` resolved via
            // `retrieval_jobs` above), or the chained form
            // (`df = store.get_historical_features(...).to_df()`)
            // in one statement. Anything else calling `.to_df()`
            // (unrelated to Feast) is deliberately left alone —
            // matched only once one of these two specific shapes is
            // confirmed, not on the method name alone.
            self.assign_feast_retrieval(errors, assign, current_line, current_col, attr);
        } else if SQL_FINALIZE_METHODS.contains(&func_name) {
            // client.query(sql).to_dataframe() / spark.sql(sql)
            // .toPandas() / duckdb.sql(sql).df()/.pl() — only
            // dispatched once the call this is chained onto is
            // confirmed to be one of SQL_PRODUCING_METHODS with a
            // resolvable SQL argument; `.df()`/`.pl()`/`.toPandas()`
            // alone are too generic a signal (plenty of unrelated
            // code has methods by those names) to count or warn on
            // by name alone.
            self.assign_sql_finalize(errors, assign, current_line, current_col, attr);
        } else if func_name == "fetch_pandas_all" {
            // cursor.fetch_pandas_all() — the second half of the
            // Snowflake/Redshift cursor pattern; the SQL text was
            // recorded by the `cursor.execute(sql)` bare-statement
            // handling above. Unlike the finalize methods above,
            // this name is specific enough that dispatching on it
            // alone (even with no prior tracked `execute()`) is
            // safe — it isn't a generic accessor name plausibly
            // used for something unrelated.
            self.assign_cursor_fetch(errors, assign, current_line, current_col, attr);
        } else if func_name == "drop" {
            self.assign_drop(errors, assign, current_line, current_col, attr, call);
        } else if func_name == "rename" {
            self.assign_rename(errors, assign, current_line, current_col, attr, call);
        } else if let Some(op) = self.polars_frame_op(attr, call) {
            self.assign_polars_frame_op(errors, assign, current_line, current_col, op);
        } else if func_name == "assign"
            // `with_columns` is a common method name: only a receiver
            // already tracked as a frame makes it the polars one.
            || (func_name == "with_columns"
                && matches!(&*attr.value, Expr::Name(r)
                    if self.variables.contains_key(r.id.as_str())))
        {
            self.assign_added_columns(assign, current_line, current_col, attr, func_name, call);
        } else if func_name == "pop" {
            // pop('col') removes a column in-place and returns a Series.
            // Mutate the receiver's schema; do not track the assignment target.
            self.assign_pop(errors, current_line, current_col, attr, call);
        } else if func_name == "insert" {
            // insert(loc, col, value) adds a column in-place; returns None.
            // Mutate the receiver's schema; do not track the assignment target.
            self.assign_insert(current_line, current_col, attr, call);
        }
    }

    fn visit_try(&mut self, errors: &mut Vec<LintError>, try_stmt: &ruff_python_ast::StmtTry) {
        self.visit_body(&try_stmt.body, errors);
        for handler in &try_stmt.handlers {
            let ast::ExceptHandler::ExceptHandler(h) = handler;
            self.visit_body(&h.body, errors);
        }
        self.visit_body(&try_stmt.orelse, errors);
        self.visit_body(&try_stmt.finalbody, errors);
    }

    fn visit_for(&mut self, errors: &mut Vec<LintError>, for_stmt: &ruff_python_ast::StmtFor) {
        self.visit_expr(&for_stmt.iter, errors);
        let entered = self.enter_loop(for_stmt);
        self.visit_body(&for_stmt.body, errors);
        if entered {
            self.loop_bindings.pop();
        }
        self.visit_body(&for_stmt.orelse, errors);
    }

    fn assign_polars_frame_op(
        &mut self,
        errors: &mut Vec<LintError>,
        assign: &ruff_python_ast::StmtAssign,
        current_line: usize,
        current_col: usize,
        op: PolarsFrameOp,
    ) {
        self.apply_polars_frame_op(assign, op, current_line, current_col, errors);
    }

    fn visit_if(&mut self, errors: &mut Vec<LintError>, if_stmt: &ruff_python_ast::StmtIf) {
        self.visit_expr(&if_stmt.test, errors);
        self.visit_body(&if_stmt.body, errors);
        for clause in &if_stmt.elif_else_clauses {
            if let Some(test) = &clause.test {
                self.visit_expr(test, errors);
            }
            self.visit_body(&clause.body, errors);
        }
    }

    fn assign_feast_job_frame(
        &mut self,
        assign: &ruff_python_ast::StmtAssign,
        call: &ruff_python_ast::ExprCall,
    ) {
        if let Some(cols) = self.extract_feast_feature_columns(call) {
            let entity_cols = self.resolve_feast_entity_columns(call);
            if let [Expr::Name(target_name)] = assign.targets.as_slice() {
                self.retrieval_jobs
                    .insert(target_name.id.to_string(), (cols, entity_cols));
            }
        }
    }
}

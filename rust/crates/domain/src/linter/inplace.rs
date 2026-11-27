use super::*;

impl Linter {
    // `df.method(..., inplace=True)` as a statement. Methods that keep the columns are
    // left alone; `drop`/`rename` with literal arguments produce the resulting column
    // set; anything else leaves the frame's columns unknown from here.
    pub(super) fn apply_inplace_method(
        &mut self,
        attr: &ast::ExprAttribute,
        call: &ast::ExprCall,
        line: usize,
        col: usize,
        errors: &mut Vec<LintError>,
    ) {
        let Expr::Name(recv) = &*attr.value else {
            return;
        };
        let recv = recv.id.as_str();
        let Some((schema_name, defined_line)) = self.variables.get(recv).cloned() else {
            return;
        };
        let method = attr.attr.as_str();
        if !self.runs_in_place(call)
            || ROW_PASSTHROUGH_METHODS.contains(&method)
            || SCHEMA_PRESERVING_METHODS.contains(&method)
        {
            return;
        }
        let base_cols = if self.schema_is_unresolved(&schema_name) {
            None
        } else {
            self.schemas.get(&schema_name).cloned()
        };
        let schema_display = self.schema_display(&schema_name, defined_line);
        let new_cols =
            inplace_method_columns(call, line, col, errors, method, base_cols, schema_display);
        if new_cols.as_ref() == self.schemas.get(&schema_name) && new_cols.is_some() {
            return;
        }
        let resolved = new_cols.is_some();
        let was_unresolved = self.schema_is_unresolved(&schema_name);
        let new_schema = self.count_mutation_leg(Some(&schema_name), new_cols, recv, line, col);
        self.variables.insert(recv.to_string(), (new_schema, line));
        if !resolved && !was_unresolved {
            self.record_leg_event(
                recv,
                line,
                col,
                "unresolved",
                format!("`.{method}(inplace=True)` is not modelled, so the columns are unknown from here"),
            );
        }
    }

    // The value of `expr` when it only reads `frame`'s known columns and fixed strings
    // (`[c.lower() for c in df.columns]`).
    fn eval_over_columns(
        frame: &str,
        columns: &[String],
        expr: &Expr,
    ) -> Option<static_eval::Value> {
        let resolver = static_eval::FrameColumns { frame, columns };
        static_eval::Evaluator::new(&resolver).eval(expr)
    }

    // `df.rename(columns={c: ... for c in df.columns})`: the mapping a non-literal
    // argument evaluates to over the frame's known columns.
    pub(super) fn evaluated_rename_mapping(
        call: &ast::ExprCall,
        frame: &str,
        columns: &[String],
    ) -> Option<HashMap<String, String>> {
        let arg = keyword(call, "columns")
            .map(|k| &k.value)
            .or_else(|| call.arguments.args.first())?;
        let static_eval::Value::Dict(items) = Self::eval_over_columns(frame, columns, arg)? else {
            return None;
        };
        items
            .into_iter()
            .map(|(k, v)| match (k, v) {
                (static_eval::Value::Str(k), static_eval::Value::Str(v)) => Some((k, v)),
                _ => None,
            })
            .collect()
    }

    // `df.columns = [...]`: a literal list of strings is the new column set, anything
    // else (other than the case-fold handled with the assignment dispatch) leaves it
    // unknown.
    pub(super) fn apply_columns_assignment(
        &mut self,
        assign: &ast::StmtAssign,
        line: usize,
        col: usize,
    ) {
        for target in &assign.targets {
            let Expr::Attribute(target_attr) = target else {
                continue;
            };
            let Expr::Name(recv) = &*target_attr.value else {
                continue;
            };
            if target_attr.attr.as_str() != "columns" {
                continue;
            }
            let recv = recv.id.as_str();
            let Some((schema_name, _)) = self.variables.get(recv).cloned() else {
                continue;
            };
            if self.schema_is_unresolved(&schema_name) {
                continue;
            }
            let base_cols = self.schemas.get(&schema_name).cloned().unwrap_or_default();
            // A comprehension over the frame's own columns, with every step fixed, is as
            // good as a literal -- only when it keeps the column count, as pandas requires.
            let literal = ast_extract::extract_string_list(&assign.value).or_else(|| {
                Self::eval_over_columns(recv, &base_cols, &assign.value)?
                    .as_string_list()
                    .filter(|cols| cols.len() == base_cols.len())
            });
            let is_case_fold = ast_extract::extract_columns_str_fold(&assign.value)
                .is_some_and(|(rhs_recv, _)| rhs_recv == recv);
            if is_case_fold {
                continue;
            }
            let resolved = literal.is_some();
            let new_schema = self.count_mutation_leg(Some(&schema_name), literal, recv, line, col);
            self.variables.insert(recv.to_string(), (new_schema, line));
            if !resolved {
                self.record_leg_event(
                    recv,
                    line,
                    col,
                    "unresolved",
                    "`.columns` is assigned a value that is not a literal list of strings"
                        .to_string(),
                );
            }
        }
    }

    // `loader = Loader(...)` makes `loader` a known instance of a class defined in this file
    // (see `receiver_classes`); any other assignment to the name forgets it.
    pub(super) fn track_instance_binding(&mut self, stmt: &Stmt) {
        let (target, value) = match stmt {
            Stmt::Assign(assign) => match assign.targets.as_slice() {
                [Expr::Name(target)] => (target, Some(&*assign.value)),
                _ => return,
            },
            Stmt::AnnAssign(ann) => match &*ann.target {
                Expr::Name(target) => (target, ann.value.as_deref()),
                _ => return,
            },
            _ => return,
        };
        let constructed = match value {
            Some(Expr::Call(call)) => match &*call.func {
                Expr::Name(class) if self.class_methods.contains_key(class.id.as_str()) => {
                    Some(class.id.to_string())
                }
                _ => None,
            },
            _ => None,
        };
        match constructed {
            Some(class) => self.receiver_classes.insert(target.id.to_string(), class),
            None => self.receiver_classes.remove(target.id.as_str()),
        };
    }

    // `del df`, `for df in ...`, `with ... as df`, `(df := ...)`: the name stops holding
    // the tracked frame. Runs before the statement's body is walked.
    pub(super) fn untrack_rebound_names(&mut self, stmt: &Stmt) {
        let (line, col) = self.source_location(stmt.range().start());
        let mut rebound: Vec<(String, &'static str)> = Vec::new();
        let mut targets: Vec<(&Expr, &'static str)> = Vec::new();
        match stmt {
            Stmt::Delete(delete) => {
                targets.extend(delete.targets.iter().map(|t| (t, "was deleted with `del`")));
            }
            Stmt::For(for_stmt) => {
                targets.push((&for_stmt.target, "is rebound as a loop variable"));
            }
            Stmt::With(with_stmt) => {
                targets.extend(
                    with_stmt
                        .items
                        .iter()
                        .filter_map(|item| item.optional_vars.as_deref())
                        .map(|t| (t, "is rebound by `with ... as`")),
                );
            }
            _ => {}
        }
        for (target, why) in targets {
            collect_name_targets(target, why, &mut rebound);
        }
        let mut walrus = WalrusCollector { names: Vec::new() };
        for expr in own_expressions(stmt) {
            walrus.visit_expr(expr);
        }
        rebound.extend(
            walrus
                .names
                .into_iter()
                .map(|name| (name, "is rebound by an assignment expression")),
        );
        for (name, why) in rebound {
            self.open_frames.remove(&name);
            if self.variables.remove(&name).is_none() {
                continue;
            }
            self.record_leg_event(&name, line, col, "untracked", format!("`{name}` {why}"));
        }
    }

    // Remove a column in-place from `recv`'s schema. Used for `del df['col']` and `df.pop('col')`.
    pub(super) fn remove_column_inplace(
        &mut self,
        recv: &str,
        col_name: &str,
        line: usize,
        col: usize,
        context: &str,
        errors: &mut Vec<LintError>,
    ) {
        let base_info = self.variables.get(recv).map(|(s, l)| (s.clone(), *l));
        let Some((schema_name, def_line)) = base_info else {
            return;
        };
        if self.schema_is_unresolved(&schema_name) {
            // Can't verify a removal against an unknown column set -- and whether the
            // named column actually existed to remove is itself unknown, so the
            // result is its own new, unresolved leg (see `count_mutation_leg`) rather
            // than a false "column doesn't exist" against an empty placeholder.
            let new_schema = self.count_mutation_leg(Some(&schema_name), None, recv, line, col);
            self.variables.insert(recv.to_string(), (new_schema, line));
            return;
        }
        let schema_display = self.schema_display(&schema_name, def_line);
        let Some(cols) = self.schemas.get(&schema_name).cloned() else {
            return;
        };
        if !cols.contains(&col_name.to_string()) {
            errors.push(LintError {
                line,
                col,
                code: CODE_UNKNOWN_COLUMN.to_string(),
                message: format!(
                    "Column '{}' does not exist in {} ({})",
                    col_name, schema_display, context
                ),
                severity: "error".to_string(),
            });
            // Nothing was actually removed (the column never existed) -- the schema
            // is unchanged, so this isn't its own new leg.
        } else {
            let new_cols: Vec<String> = cols
                .into_iter()
                .filter(|c| c.as_str() != col_name)
                .collect();
            let new_schema =
                self.count_mutation_leg(Some(&schema_name), Some(new_cols), recv, line, col);
            self.variables.insert(recv.to_string(), (new_schema, line));
        }
    }

    // Add a column in-place to `recv`'s schema. Used for `df.insert(loc, col, value)`.
    pub(super) fn add_column_inplace(
        &mut self,
        recv: &str,
        col_name: &str,
        line: usize,
        col: usize,
    ) {
        let base_info = self.variables.get(recv).map(|(s, l)| (s.clone(), *l));
        let Some((schema_name, _)) = base_info else {
            return;
        };
        if self.schema_is_unresolved(&schema_name) {
            let new_schema = self.count_mutation_leg(Some(&schema_name), None, recv, line, col);
            self.variables.insert(recv.to_string(), (new_schema, line));
            return;
        }
        let mut cols = self.schemas.get(&schema_name).cloned().unwrap_or_default();
        if !cols.contains(&col_name.to_string()) {
            cols.push(col_name.to_string());
            let new_schema =
                self.count_mutation_leg(Some(&schema_name), Some(cols), recv, line, col);
            self.variables.insert(recv.to_string(), (new_schema, line));
        }
    }
}

fn inplace_method_columns(
    call: &ruff_python_ast::ExprCall,
    line: usize,
    col: usize,
    errors: &mut Vec<LintError>,
    method: &str,
    base_cols: Option<Vec<String>>,
    schema_display: String,
) -> Option<Vec<String>> {
    let new_cols = base_cols.and_then(|base| match method {
        "drop" => match inplace_drop_columns(call) {
            InplaceColumns::Unchanged => Some(base),
            InplaceColumns::Edit(dropped) => {
                for col_name in dropped.iter().filter(|c| !base.contains(c)) {
                    errors.push(LintError {
                        line,
                        col,
                        code: CODE_DROPPED_UNKNOWN_COLUMN.to_string(),
                        message: format!(
                            "Dropped column '{}' does not exist in {}",
                            col_name, schema_display
                        ),
                        severity: "warning".to_string(),
                    });
                }
                Some(base.into_iter().filter(|c| !dropped.contains(c)).collect())
            }
            InplaceColumns::Unknown => None,
        },
        "rename" => {
            let mapping = match inplace_rename_mapping(call) {
                InplaceColumns::Unchanged => return Some(base),
                InplaceColumns::Unknown => return None,
                InplaceColumns::Edit(mapping) => mapping,
            };
            for old_col in mapping.keys().filter(|c| !base.contains(c)) {
                errors.push(LintError {
                    line,
                    col,
                    code: CODE_UNKNOWN_COLUMN.to_string(),
                    message: format!(
                        "Column '{}' does not exist in {} (rename)",
                        old_col, schema_display
                    ),
                    severity: "error".to_string(),
                });
            }
            Some(
                base.iter()
                    .map(|c| mapping.get(c).cloned().unwrap_or_else(|| c.clone()))
                    .collect(),
            )
        }
        _ => None,
    });
    new_cols
}

pub(super) fn check_renamed_columns_exist(
    errors: &mut Vec<LintError>,
    current_line: usize,
    current_col: usize,
    base_unresolved: bool,
    base_cols: Vec<String>,
    mapping: HashMap<String, String>,
    schema_display: String,
) -> Vec<String> {
    for old_col in mapping.keys().filter(|_| !base_unresolved) {
        if !base_cols.contains(old_col) {
            errors.push(LintError {
                line: current_line,
                col: current_col,
                code: CODE_UNKNOWN_COLUMN.to_string(),
                message: format!(
                    "Column '{}' does not exist in {} (rename)",
                    old_col, schema_display
                ),
                severity: "error".to_string(),
            });
        }
    }
    let new_cols: Vec<String> = base_cols
        .iter()
        .map(|c| mapping.get(c).cloned().unwrap_or_else(|| c.clone()))
        .collect();
    new_cols
}

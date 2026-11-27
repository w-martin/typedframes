use super::*;

impl Linter {
    // Validate column access expressions against known schemas.
    //
    // Checked expression kinds:
    //
    // * `Attribute` (`df.col_name`) — validates `col_name` against the schema of `df`
    //   if `df` is a tracked variable, skipping names in `RESERVED_METHODS`.
    // * `Subscript` (`df["col_name"]`) — validates the string literal key.
    // * `Call` — recurses into positional arguments and, when the callee is
    //   `receiver.method(...)`, recurses only into `receiver` rather than the method
    //   name itself.  This avoids false positives where the method name (e.g. `assign`,
    //   `groupby`) is mistakenly checked as a column.
    //
    // Typo suggestions are added via find_best_match when the edit distance to the
    // closest known column name is ≤ 2.
    // Whether `name` is tracked with a concrete column list, which the closed-set checks own.
    pub(super) fn is_closed_frame(&self, name: &str) -> bool {
        self.variables
            .get(name)
            .is_some_and(|(schema, _)| !self.schema_is_unresolved(schema))
    }

    // A read of `column` off a frame the checker knows only by name: an error if the frame
    // certainly lost it, otherwise noted for `column-read-before-assignment`.
    pub(super) fn check_open_frame_read(
        &self,
        frame: &str,
        column: &str,
        at: ruff_text_size::TextSize,
        errors: &mut Vec<LintError>,
    ) {
        if self.is_closed_frame(frame) {
            return;
        }
        let Some(open) = self.open_frames.get(frame) else {
            return;
        };
        let (line, col) = self.source_location(at);
        if let Some((why, _)) = open.absent.get(column) {
            errors.push(LintError {
                line,
                col,
                code: CODE_UNKNOWN_COLUMN.to_string(),
                message: format!(
                    "Column '{column}' does not exist in `{frame}`: it was {why} (the frame's other columns are unknown)"
                ),
                severity: "error".to_string(),
            });
        } else if !open.written.contains(column) {
            self.unknown_frame_reads
                .borrow_mut()
                .push((line, col, column.to_string()));
            self.open_reads
                .borrow_mut()
                .entry((frame.to_string(), column.to_string()))
                .or_insert((self.stmt_counter, line, col));
        }
    }

    // `frame[["a", "b"]]` on a frame whose columns are unknown, for `likely-column-typo`.
    pub(super) fn note_unknown_list_reads(&self, frame: &str, list: &Expr) {
        let unknown = match self.variables.get(frame) {
            Some((schema, _)) => self.schema_is_unresolved(schema),
            None => self.open_frames.contains_key(frame),
        };
        if !unknown {
            return;
        }
        let written = self.open_frames.get(frame).map(|o| &o.written);
        let Expr::List(items) = list else {
            return;
        };
        for item in &items.elts {
            let Some(column) = ast_extract::extract_string_literal(item) else {
                continue;
            };
            if written.is_some_and(|w| w.contains(column)) {
                continue;
            }
            let (line, col) = self.source_location(item.range().start());
            self.unknown_frame_reads
                .borrow_mut()
                .push((line, col, column.to_string()));
        }
    }

    // Keeps `open_frames` in step with the statement just visited: a new name for a frame
    // the checker could only recognise, a derived one, edits that remove columns, and the
    // writes that bring them back (or give the frame a column a read ran ahead of).
    pub(super) fn update_open_frames(
        &mut self,
        stmt: &Stmt,
        sites_before: usize,
        errors: &mut Vec<LintError>,
    ) {
        let (line, stmt_col) = self.source_location(stmt.range().start());
        let depth = self.block_depth;
        match stmt {
            Stmt::Assign(assign) => {
                if let ControlFlow::Break(_) =
                    self.update_open_frames_assign(sites_before, errors, line, stmt_col, assign)
                {
                    return;
                }
            }
            Stmt::AnnAssign(ann) => {
                if let (Expr::Name(target), Some(value)) = (&*ann.target, ann.value.as_deref()) {
                    self.rebind_open_frame(
                        target.id.as_str(),
                        value,
                        sites_before,
                        (line, stmt_col),
                    );
                }
            }
            Stmt::Delete(delete) => {
                self.update_open_frames_delete(line, depth, delete);
            }
            Stmt::Expr(expr_stmt) => {
                if let Expr::Call(call) = &*expr_stmt.value {
                    self.apply_call_to_open_frames(call, line);
                }
            }
            _ => {}
        }
        self.forget_frames_handed_to_helpers(stmt);
    }

    fn update_open_frames_assign(
        &mut self,
        sites_before: usize,
        errors: &mut Vec<LintError>,
        line: usize,
        stmt_col: usize,
        assign: &ruff_python_ast::StmtAssign,
    ) -> ControlFlow<()> {
        for target in &assign.targets {
            self.note_open_frame_write(target, &assign.value, line, errors);
        }
        let [Expr::Name(target)] = assign.targets.as_slice() else {
            for target in &assign.targets {
                let mut names = Vec::new();
                collect_name_targets(target, "", &mut names);
                for (name, _) in names {
                    self.open_frames.remove(&name);
                }
            }
            return ControlFlow::Break(());
        };
        self.note_merge_after_read(target.id.as_str(), &assign.value, line, errors);
        self.note_derived_write_after_read(target.id.as_str(), &assign.value, line, errors);
        self.rebind_open_frame(
            target.id.as_str(),
            &assign.value,
            sites_before,
            (line, stmt_col),
        );
        ControlFlow::Continue(())
    }

    fn update_open_frames_delete(
        &mut self,
        line: usize,
        depth: usize,
        delete: &ruff_python_ast::StmtDelete,
    ) {
        for target in &delete.targets {
            self.forget_deleted_columns(line, depth, target);
        }
    }

    fn forget_deleted_columns(&mut self, line: usize, depth: usize, target: &Expr) {
        if let Expr::Subscript(sub) = target {
            self.update_open_frames_subscript(line, depth, sub);
        }
    }

    fn update_open_frames_subscript(
        &mut self,
        line: usize,
        depth: usize,
        sub: &ruff_python_ast::ExprSubscript,
    ) {
        if let (Expr::Name(frame), Some(column)) =
            (&*sub.value, ast_extract::extract_string_literal(&sub.slice))
        {
            if let Some(open) = self.open_frames.get_mut(frame.id.as_str()) {
                if open.depth == depth {
                    open.absent.insert(
                        column.to_string(),
                        (format!("deleted at line {line}"), line),
                    );
                }
            }
        }
    }
}

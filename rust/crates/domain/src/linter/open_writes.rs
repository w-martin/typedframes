use super::*;

impl Linter {
    // `frame["c"] = ...`, `frame[["a", "b"]] = ...`, `frame.loc[:, "c"] = ...` and
    // `frame.columns = ...` on a frame known only by name.
    pub(super) fn note_open_frame_write(
        &mut self,
        target: &Expr,
        value: &Expr,
        line: usize,
        errors: &mut Vec<LintError>,
    ) {
        let (frame_name, columns) = match self.note_open_frame_attribute(target) {
            Some(value) => value,
            None => return,
        };
        let reads_itself = |column: &str| {
            let mut reads = NoteOpenFrameWriteReads {
                frame: &frame_name,
                column,
                found: false,
            };
            reads.visit_expr(value);
            reads.found
        };
        for column in columns {
            let early_read = self
                .open_reads
                .borrow()
                .get(&(frame_name.clone(), column.clone()))
                .copied();
            let self_read = reads_itself(&column);
            let at_frame_depth = self
                .open_frames
                .get(&frame_name)
                .is_some_and(|open| open.depth == self.block_depth);
            let Some(open) = self.open_frames.get_mut(&frame_name) else {
                return;
            };
            open.absent.remove(&column);
            let first_mention = open.written.insert(column.clone());
            if let (true, true, false, Some((stmt, read_line, read_col))) =
                (first_mention, at_frame_depth, self_read, early_read)
            {
                if stmt < self.stmt_counter {
                    errors.push(LintError {
                        line: read_line,
                        col: read_col,
                        code: CODE_READ_BEFORE_ASSIGNMENT.to_string(),
                        message: format!(
                            "Column '{column}' is read here, but `{frame_name}` only gets it at line {line}; unless the data already has it, this raises KeyError"
                        ),
                        severity: "warning".to_string(),
                    });
                }
            }
        }
    }

    fn note_open_frame_attribute(&mut self, target: &Expr) -> Option<(String, Vec<String>)> {
        if let Expr::Attribute(attr) = target {
            self.note_open_frame_write_attribute(attr);
            return None;
        }
        let Expr::Subscript(sub) = target else {
            return None;
        };
        let frame_name = match &*sub.value {
            Expr::Name(n) => n.id.to_string(),
            Expr::Attribute(a) => match &*a.value {
                Expr::Name(n) => n.id.to_string(),
                _ => return None,
            },
            _ => return None,
        };
        if !self.open_frames.contains_key(&frame_name) {
            return None;
        }
        let Some((_, columns)) = column_write_target(target) else {
            if let Some(open) = self.open_frames.get_mut(&frame_name) {
                open.absent.clear();
            }
            return None;
        };
        Some((frame_name, columns))
    }

    fn note_open_frame_write_attribute(&mut self, attr: &ruff_python_ast::ExprAttribute) {
        if let Expr::Name(frame) = &*attr.value {
            if attr.attr.as_str() == "columns" {
                // Every earlier name may have been renamed, but a drop or rename made after
                // the assignment is certain again.
                if let Some(open) = self.open_frames.get_mut(frame.id.as_str()) {
                    open.absent.clear();
                }
            } else if let Some(open) = self.open_frames.get_mut(frame.id.as_str()) {
                open.absent.remove(attr.attr.as_str());
                open.written.insert(attr.attr.to_string());
            }
        }
    }

    // `frame.drop(columns=[..], inplace=True)`, `frame.rename(.., inplace=True)`,
    // `frame.pop("c")`, `frame.insert(i, "c", ..)` as statements.
    pub(super) fn apply_call_to_open_frames(&mut self, call: &ast::ExprCall, line: usize) {
        let Expr::Attribute(attr) = &*call.func else {
            return;
        };
        let Expr::Name(frame) = &*attr.value else {
            return;
        };
        let name = frame.id.as_str();
        let depth = self.block_depth;
        let Some(open) = self.open_frames.get_mut(name) else {
            return;
        };
        let certain = open.depth == depth;
        match attr.attr.as_str() {
            "drop" if inplace_literal(call) == Some(true) => {
                if let (true, InplaceColumns::Edit(dropped)) = (certain, inplace_drop_columns(call))
                {
                    for column in dropped {
                        open.absent
                            .insert(column, (format!("dropped at line {line}"), line));
                    }
                }
            }
            "rename" if inplace_literal(call) == Some(true) => match inplace_rename_mapping(call) {
                InplaceColumns::Edit(mapping) => {
                    for new in mapping.values() {
                        open.absent.remove(new);
                    }
                    if certain {
                        for (old, new) in mapping.iter().filter(|(o, n)| o != n) {
                            open.absent.insert(
                                old.clone(),
                                (format!("renamed to '{new}' at line {line}"), line),
                            );
                        }
                    }
                }
                InplaceColumns::Unchanged => {}
                InplaceColumns::Unknown => {
                    self.open_frames.remove(name);
                }
            },
            "pop" => {
                if let (true, Some(column)) = (
                    certain,
                    call.arguments
                        .args
                        .first()
                        .and_then(ast_extract::extract_string_literal),
                ) {
                    open.absent
                        .insert(column.to_string(), (format!("popped at line {line}"), line));
                }
            }
            "insert" => {
                note_inserted_column(call, open);
            }
            // Row-wise in-place edits leave the columns alone (an index reset may add one).
            method
                if keyword(call, "inplace")
                    .is_some_and(|k| !matches!(&k.value, Expr::BooleanLiteral(b) if !b.value))
                    && (ROW_PASSTHROUGH_METHODS.contains(&method)
                        || SCHEMA_PRESERVING_METHODS.contains(&method)) =>
            {
                let keep_absent = method != "reset_index" || resets_index_dropping(call);
                open.absent.retain(|_, _| keep_absent);
            }
            _ if keyword(call, "inplace")
                .is_some_and(|k| !matches!(&k.value, Expr::BooleanLiteral(b) if !b.value)) =>
            {
                self.open_frames.remove(name);
            }
            _ => {}
        }
    }

    // A frame handed to a function the checker cannot see into may come back with columns
    // the frame had lost.
    pub(super) fn forget_frames_handed_to_helpers(&mut self, stmt: &Stmt) {
        if self.open_frames.is_empty() {
            return;
        }
        let mut handed = Handed {
            frames: &self.open_frames,
            file_methods: &self.file_function_names,
            handed: Vec::new(),
        };
        for expr in own_expressions(stmt) {
            handed.visit_expr(expr);
        }
        let handed = handed.handed;
        for name in handed {
            if let Some(open) = self.open_frames.get_mut(&name) {
                open.absent.clear();
            }
        }
    }
}

fn note_inserted_column(call: &ruff_python_ast::ExprCall, open: &mut OpenFrame) {
    match call
        .arguments
        .args
        .get(1)
        .and_then(ast_extract::extract_string_literal)
    {
        Some(column) => {
            open.absent.remove(column);
            open.written.insert(column.to_string());
        }
        None => open.absent.clear(),
    }
}

// Columns an edit writes into an open frame: no longer absent.
pub(super) fn note_written(open: &mut OpenFrame, names: Vec<String>) {
    for name in names {
        open.absent.remove(name.as_str());
        open.written.insert(name);
    }
}

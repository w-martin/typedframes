use super::*;

impl Linter {
    // The frame `expr` evaluates to when it derives from a frame known only by name through
    // edits whose arguments are literal; `None` as soon as a step could bring a column back.
    pub(super) fn open_after(&self, expr: &Expr, line: usize) -> Option<OpenFrame> {
        match expr {
            Expr::Name(name) if !self.is_closed_frame(name.id.as_str()) => {
                self.open_frames.get(name.id.as_str()).cloned()
            }
            // `frame[mask]` / `frame.loc[mask]`: the same columns, fewer rows.
            Expr::Subscript(sub) => self.open_after_subscript(line, sub),
            Expr::Call(call) => self.open_after_call(expr, call, line),
            _ => None,
        }
    }

    // `open_after` for a call: a load or a known class's method returning a frame, or an edit of an
    // open frame.
    fn open_after_call(&self, expr: &Expr, call: &ast::ExprCall, line: usize) -> Option<OpenFrame> {
        let callee = match &*call.func {
            Expr::Name(n) => n.id.as_str(),
            Expr::Attribute(a) => a.attr.as_str(),
            _ => return None,
        };
        // `self.repo.load()`: a method the attribute's class (a Protocol's
        // implementors included) declares to return a frame.
        if let Some(value) = self.open_after_method_attribute_class(call, callee) {
            return value;
        }
        // `pl.from_pandas(frame)`: the same columns in another library.
        if callee == "from_pandas"
            && matches!(&*call.func, Expr::Attribute(a)
            if matches!(&*a.value, Expr::Name(m) if matches!(m.id.as_str(), "pl" | "polars")))
        {
            return self.open_after(call.arguments.args.first()?, line);
        }
        if callee == "get_dummies" {
            let replaced = open_after_callee_get_dummies(call)?;
            return Some(OpenFrame {
                depth: self.block_depth,
                absent: replaced
                    .into_iter()
                    .map(|c| {
                        (
                            c,
                            (format!("replaced by dummy columns at line {line}"), line),
                        )
                    })
                    .collect(),
                written: Default::default(),
            });
        }
        let Expr::Attribute(attr) = &*call.func else {
            return None;
        };
        // Pivoted column names come from the data, so even a known frame pivots to an
        // open one.
        let columns_edit = match self.open_after_pivoted_column_names(call, attr) {
            Ok(value) => value,
            Err(value) => return value,
        };
        let open = match self.open_after(&attr.value, line) {
            Some(open) => open,
            None if columns_edit => OpenFrame {
                depth: self.block_depth,
                ..OpenFrame::default()
            },
            None => return None,
        };
        if keyword(call, "inplace").is_some() {
            return None;
        }
        self.apply_open_frame_method(expr, call, attr, line, open)
    }

    // What the method `attr` names does to the open frame `open`; `None` when it may bring any
    // column back.
    fn apply_open_frame_method(
        &self,
        expr: &Expr,
        call: &ast::ExprCall,
        attr: &ast::ExprAttribute,
        line: usize,
        mut open: OpenFrame,
    ) -> Option<OpenFrame> {
        let method = attr.attr.as_str();
        match method {
            "drop" => {
                self.open_after_drop(line, call, attr, &mut open);
            }
            "rename" => {
                let mapping = ast_extract::extract_rename_mapping(call)?;
                open_after_rename(line, &mut open, mapping);
            }
            "assign" => match ast_extract::assign_output_names(call) {
                Some(names) => note_written(&mut open, names),
                None => open.absent.clear(),
            },
            "with_columns" => match self.with_columns_outputs(call) {
                Some(names) => note_written(&mut open, names),
                // Any column may have been added back.
                None => open.absent.clear(),
            },
            // The index (whatever its names) comes back as columns.
            "reset_index" if !resets_index_dropping(call) => open.absent.clear(),
            _ if ROW_PASSTHROUGH_METHODS.contains(&method)
                || SCHEMA_PRESERVING_METHODS.contains(&method) => {}
            // The result keeps the left frame's columns; a dropped one stays gone unless
            // the right frame is known to bring it back.
            "join" | "merge" => {
                self.open_after_join_merge(call, &mut open);
            }
            // Certainly a frame again, with columns nothing here says anything about.
            _ if is_reshaping_call(expr) => {
                open = OpenFrame {
                    depth: self.block_depth,
                    ..OpenFrame::default()
                };
            }
            _ => return None,
        }
        Some(open)
    }

    fn open_after_pivoted_column_names(
        &self,
        call: &ruff_python_ast::ExprCall,
        attr: &ruff_python_ast::ExprAttribute,
    ) -> Result<bool, Option<OpenFrame>> {
        if matches!(attr.attr.as_str(), "pivot" | "pivot_table")
            && self.expr_frame_columns(&attr.value).is_some()
        {
            return Err(Some(OpenFrame {
                depth: self.block_depth,
                ..OpenFrame::default()
            }));
        }
        let columns_edit = matches!(attr.attr.as_str(), "drop" | "rename")
            && keyword(call, "columns").is_some()
            && self.expr_frame_columns(&attr.value).is_none();
        Ok(columns_edit)
    }

    fn open_after_join_merge(&self, call: &ruff_python_ast::ExprCall, open: &mut OpenFrame) {
        let right = call
            .arguments
            .args
            .first()
            .or_else(|| keyword(call, "other").map(|k| &k.value))
            .or_else(|| keyword(call, "right").map(|k| &k.value));
        match right.and_then(|r| self.expr_frame_columns(r)) {
            Some((columns, _)) => {
                open.absent.retain(|c, _| !columns.contains(c));
                open.written.extend(columns);
            }
            None => open.absent.clear(),
        }
    }

    fn open_after_subscript(
        &self,
        line: usize,
        sub: &ruff_python_ast::ExprSubscript,
    ) -> Option<OpenFrame> {
        let (base, slice) = match &*sub.value {
            Expr::Attribute(a) if matches!(a.attr.as_str(), "loc" | "iloc") => {
                (&*a.value, &*sub.slice)
            }
            other => (other, &*sub.slice),
        };
        let is_row_mask = matches!(
            slice,
            Expr::Compare(_) | Expr::BoolOp(_) | Expr::UnaryOp(_) | Expr::BinOp(_) | Expr::Slice(_)
        ) || is_series_method_chain(slice);
        if !is_row_mask {
            return None;
        }
        self.open_after(base, line)
    }

    fn open_after_method_attribute_class(
        &self,
        call: &ruff_python_ast::ExprCall,
        callee: &str,
    ) -> Option<Option<OpenFrame>> {
        if let Expr::Attribute(method) = &*call.func {
            if let Some(value) = self.open_after_method_attribute(method) {
                return value;
            }
        }
        if matches!(&*call.func, Expr::Name(_))
            && self
                .functions
                .get(callee)
                .is_some_and(|s| s == OPEN_FRAME_MARKER)
        {
            return Some(Some(OpenFrame {
                depth: self.block_depth,
                ..OpenFrame::default()
            }));
        }
        // A helper annotated to return a bare DataFrame: certainly a frame.
        // `pd.merge(a, b)`, `pd.merge_asof(a, b, ..)`: certainly a frame, with columns of
        // the operands nothing here says anything about.
        if let Expr::Attribute(module_call) = &*call.func {
            if matches!(&*module_call.value, Expr::Name(m) if LOAD_MODULES.contains(&m.id.as_str()))
                && matches!(callee, "merge" | "merge_asof" | "merge_ordered")
            {
                return Some(Some(OpenFrame {
                    depth: self.block_depth,
                    ..OpenFrame::default()
                }));
            }
        }
        None
    }

    fn open_after_method_attribute(
        &self,
        method: &ruff_python_ast::ExprAttribute,
    ) -> Option<Option<Option<OpenFrame>>> {
        if let Expr::Attribute(recv) = &*method.value {
            if matches!(&*recv.value, Expr::Name(n) if n.id.as_str() == "self") {
                let returns_frame = self
                    .current_self_attrs
                    .get(recv.attr.as_str())
                    .and_then(|origin| {
                        self.resolve_method_on_class(&origin.class_name, method.attr.as_str())
                    })
                    .is_some();
                if returns_frame {
                    return Some(Some(Some(OpenFrame {
                        depth: self.block_depth,
                        ..OpenFrame::default()
                    })));
                }
            }
        }
        None
    }

    fn open_after_drop(
        &self,
        line: usize,
        call: &ruff_python_ast::ExprCall,
        attr: &ruff_python_ast::ExprAttribute,
        open: &mut OpenFrame,
    ) {
        if let Some(dropped) =
            ast_extract::extract_drop_columns(call, self.is_polars_receiver(&attr.value))
        {
            for column in dropped {
                open.absent
                    .insert(column, (format!("dropped at line {line}"), line));
            }
        }
    }
}

fn open_after_callee_get_dummies(call: &ruff_python_ast::ExprCall) -> Option<Vec<String>> {
    let mut replaced =
        ast_extract::extract_string_list_or_single(&keyword(call, "columns")?.value)?;
    if keyword(call, "prefix").is_some() || keyword(call, "prefix_sep").is_some() {
        replaced.clear();
    }
    Some(replaced)
}

fn open_after_rename(line: usize, open: &mut OpenFrame, mapping: HashMap<String, String>) {
    for new in mapping.values() {
        open.absent.remove(new);
        open.written.insert(new.clone());
    }
    for (old, new) in mapping.iter().filter(|(o, n)| o != n) {
        open.absent.insert(
            old.clone(),
            (format!("renamed to '{new}' at line {line}"), line),
        );
    }
}

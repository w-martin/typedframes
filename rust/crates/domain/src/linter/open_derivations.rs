use super::*;

impl Linter {
    // `df = df.merge(right, on=..)` with `right` bringing a non-key column `df` was read for
    // earlier: had `df` held it already, the merge would have suffixed both copies.
    pub(super) fn note_merge_after_read(
        &mut self,
        target: &str,
        value: &Expr,
        line: usize,
        errors: &mut Vec<LintError>,
    ) {
        let Expr::Call(call) = value else {
            return;
        };
        let Expr::Attribute(attr) = &*call.func else {
            return;
        };
        let polars_join = attr.attr.as_str() == "join" && self.is_polars_receiver(&attr.value);
        if !(attr.attr.as_str() == "merge" || polars_join)
            || !matches!(&*attr.value, Expr::Name(n) if n.id.as_str() == target)
            || keyword(call, "suffixes").is_some()
            || keyword(call, "suffix").is_some()
        {
            return;
        }
        let Some(keys) =
            keyword(call, "on").and_then(|k| ast_extract::literal_key_columns(&k.value))
        else {
            return;
        };
        let Some(open) = self.open_frames.get(target) else {
            return;
        };
        if open.depth != self.block_depth {
            return;
        }
        let Some(right) = call
            .arguments
            .args
            .first()
            .or_else(|| keyword(call, "right").map(|k| &k.value))
            .or_else(|| keyword(call, "other").map(|k| &k.value))
        else {
            return;
        };
        let Some((columns, _)) = self.expr_frame_columns(right) else {
            return;
        };
        let method = attr.attr.as_str();
        for column in columns {
            if keys.contains(&column) || open.written.contains(&column) {
                continue;
            }
            let early_read = self
                .open_reads
                .borrow()
                .get(&(target.to_string(), column.clone()))
                .copied();
            if let Some((stmt, read_line, read_col)) = early_read {
                if stmt < self.stmt_counter {
                    errors.push(LintError {
                        line: read_line,
                        col: read_col,
                        code: CODE_READ_BEFORE_ASSIGNMENT.to_string(),
                        message: format!(
                            "Column '{column}' is read here, but `{target}` only gets it from the `.{method}()` at line {line}; had it been there already, the {method} would have suffixed it"
                        ),
                        severity: "warning".to_string(),
                    });
                }
            }
        }
    }

    // `df = df.with_columns(..alias("c"))` / `df = df.assign(c=..)` after a read of `c` on `df`:
    // the same read-before-assignment as `df["c"] = ..`, unless the call itself reads `c`.
    pub(super) fn note_derived_write_after_read(
        &mut self,
        target: &str,
        value: &Expr,
        line: usize,
        errors: &mut Vec<LintError>,
    ) {
        let Expr::Call(call) = value else {
            return;
        };
        let Expr::Attribute(attr) = &*call.func else {
            return;
        };
        if !matches!(&*attr.value, Expr::Name(n) if n.id.as_str() == target) {
            return;
        }
        let names = match attr.attr.as_str() {
            "with_columns" => self.with_columns_outputs(call),
            "assign" => ast_extract::assign_output_names(call),
            _ => None,
        };
        let Some(names) = names else {
            return;
        };
        let Some(open) = self.open_frames.get(target) else {
            return;
        };
        if open.depth != self.block_depth {
            return;
        }
        let mut mentions = Mentions(Vec::new());
        for arg in &call.arguments.args {
            mentions.visit_expr(arg);
        }
        for kw in &call.arguments.keywords {
            mentions.visit_expr(&kw.value);
        }
        let method = attr.attr.as_str();
        for column in names {
            if open.written.contains(&column) || mentions.0.contains(&column) {
                continue;
            }
            let early_read = self
                .open_reads
                .borrow()
                .get(&(target.to_string(), column.clone()))
                .copied();
            if let Some((stmt, read_line, read_col)) = early_read {
                if stmt < self.stmt_counter {
                    errors.push(LintError {
                        line: read_line,
                        col: read_col,
                        code: CODE_READ_BEFORE_ASSIGNMENT.to_string(),
                        message: format!(
                            "Column '{column}' is read here, but `{target}` only gets it from the `.{method}()` at line {line}; unless the data already has it, this raises an error"
                        ),
                        severity: "warning".to_string(),
                    });
                }
            }
        }
    }

    // A new name for a frame the checker follows, bound to something it cannot model: say so,
    // so `--coverage-detail=explain` can answer where the frame went.
    pub(super) fn note_unmodelled_derivation(
        &mut self,
        name: &str,
        value: &Expr,
        line: usize,
        col: usize,
    ) {
        // Name the step that loses the columns, not a column-preserving call after it.
        let mut value = value;
        while let Expr::Call(call) = value {
            match &*call.func {
                Expr::Attribute(a)
                    if (ROW_PASSTHROUGH_METHODS.contains(&a.attr.as_str())
                        || SCHEMA_PRESERVING_METHODS.contains(&a.attr.as_str()))
                        && matches!(&*a.value, Expr::Call(_)) =>
                {
                    value = &a.value;
                }
                _ => break,
            }
        }
        let what = describe_rhs(value);
        if Self::is_series_eval(value) {
            return;
        }
        let (unknown_left, unknown_right) = match value {
            Expr::Call(call) => match &*call.func {
                Expr::Attribute(attr) if matches!(attr.attr.as_str(), "join" | "merge") => {
                    let right = call.arguments.args.first().or_else(|| {
                        keyword(call, "right")
                            .or(keyword(call, "other"))
                            .map(|k| &k.value)
                    });
                    (
                        self.expr_frame_columns(&attr.value).is_none(),
                        right.is_some_and(|r| self.expr_frame_columns(r).is_none()),
                    )
                }
                _ => (false, false),
            },
            _ => (false, false),
        };
        let pivot = matches!(value, Expr::Call(call) if matches!(&*call.func,
            Expr::Attribute(a) if matches!(a.attr.as_str(), "pivot" | "pivot_table")));
        // `agg(["sum"])` on a frame, or `agg({"v": ["min", "max"]})`: one column level per
        // function.
        let grouping = |e: &Expr| {
            let e = match e {
                Expr::Subscript(sub) => &*sub.value,
                other => other,
            };
            matches!(e, Expr::Call(g) if matches!(&*g.func,
                Expr::Attribute(ga) if matches!(ga.attr.as_str(), "groupby" | "resample")))
        };
        let multi_index = matches!(value, Expr::Call(call) if matches!(&*call.func,
        Expr::Attribute(a) if matches!(a.attr.as_str(), "agg" | "aggregate") && grouping(&a.value))
        && call.arguments.args.first().is_some_and(|arg| match arg {
            Expr::List(_) => true,
            Expr::Dict(d) => d.items.iter().any(|i| matches!(i.value, Expr::List(_) | Expr::Tuple(_))),
            _ => false,
        }));
        let reason = if multi_index {
            format!("{what} gives MultiIndex columns (one level per function), which are not modelled, so the columns of `{name}` are unknown")
        } else if pivot {
            format!("{what} takes its new column names from the data, so the columns of `{name}` are unknown")
        } else if unknown_left {
            format!("the left side of {what} has unknown columns, so the columns of `{name}` are unknown")
        } else if unknown_right {
            format!("the right side of {what} has unknown columns, so the columns of `{name}` are unknown")
        } else {
            format!("{what} is not modelled, so the columns of `{name}` are unknown")
        };
        self.record_leg_event(name, line, col, "untracked", reason);
    }

    // `frame.eval("a + b")`: no assignment, so the result is a Series, not a frame.
    fn is_series_eval(value: &Expr) -> bool {
        let Expr::Call(call) = value else {
            return false;
        };
        matches!(&*call.func, Expr::Attribute(a) if a.attr.as_str() == "eval")
            && call
                .arguments
                .args
                .first()
                .and_then(ast_extract::extract_string_literal)
                .and_then(query_names::query_names)
                .is_some_and(|names| names.assigned.is_empty())
    }

    pub(super) fn rebind_open_frame(
        &mut self,
        name: &str,
        value: &Expr,
        sites_before: usize,
        (line, col): (usize, usize),
    ) {
        let unresolved = self.variables.contains_key(name) && !self.is_closed_frame(name);
        if self.is_closed_frame(name) {
            self.open_frames.remove(name);
            return;
        }
        let fresh = || OpenFrame {
            depth: self.block_depth,
            ..OpenFrame::default()
        };
        // A rebinding under a branch leaves the name with either value afterwards.
        let conditional_rebind = self
            .open_frames
            .get(name)
            .is_some_and(|open| open.depth < self.block_depth);
        let derived = self.open_after(value, line);
        // Reads of an earlier frame bound to this name say nothing about the new one.
        if derived.is_none() {
            self.open_reads
                .borrow_mut()
                .retain(|(frame, _), _| frame != name);
        }
        // A new untyped site is an origin unless the frame was derived from one already known.
        if derived.is_none() && self.untyped_sites.len() > sites_before {
            self.open_frames.insert(name.to_string(), fresh());
            return;
        }
        match derived {
            Some(mut open) if !conditional_rebind => {
                open.depth = self.block_depth;
                if open.absent.is_empty() && open.written.is_empty() && is_reshaping_call(value) {
                    self.note_unmodelled_derivation(name, value, line, col);
                }
                self.open_frames.insert(name.to_string(), open);
            }
            // The new value holds for the rest of this block; leaving it, either value may
            // reach what follows (see `merge_shadowed_frames`).
            Some(mut new) => {
                let depth = self.block_depth;
                if let Some(old) = self.open_frames.get(name).cloned() {
                    if !self
                        .shadowed_frames
                        .iter()
                        .any(|(d, n, _)| *d == depth && n == name)
                    {
                        self.shadowed_frames.push((depth, name.to_string(), old));
                    }
                }
                new.depth = depth;
                self.open_frames.insert(name.to_string(), new);
            }
            // A frame the checker holds as unresolved is a frame whatever produced it.
            _ if unresolved => {
                self.open_frames.insert(name.to_string(), fresh());
            }
            other => {
                self.open_frames.remove(name);
                if other.is_none() && !self.variables.contains_key(name) {
                    let frames = &self.open_frames;
                    let derived = classify_rhs(value, &|n| frames.contains_key(n));
                    if derived.is_some_and(|d| d.root != name && d.kind == Kind::Derived) {
                        self.note_unmodelled_derivation(name, value, line, col);
                    }
                }
            }
        }
    }
}

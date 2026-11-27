use super::*;

pub(super) struct UsageScan<'a> {
    pub(super) helpers: &'a dyn HelperResolver,
    pub(super) tainted: HashSet<String>,
    // Nesting inside if/for/while/with/try/match -- marks an access/use `conditional`
    // rather than something every execution path is known to reach.
    pub(super) depth: u32,
    // Columns assigned into the frame on every path so far (`df["new"] = ...`): a later
    // read of one is not a read of the source, so it isn't recorded as an access.
    pub(super) written: HashSet<String>,
    // Columns a rename or drop on a tracked name took away: a later read of one fails at
    // runtime, so narrowing the load to it would bless the bug.
    pub(super) gone: HashSet<String>,
    pub(super) usage: ColumnUsage,
    // While walking a tainted-receiver subscript's slice (`df[<slice>]`), the
    // receiver's own bare name is already summarized by the observation just
    // recorded for the whole subscript (specific columns, or "this subscript") --
    // set to that receiver's name for the duration of the slice walk so a bare
    // self-reference found inside it (`df[df > 0]`, `df[df.values]`) isn't ALSO
    // recorded as a second, separate use of the same logical operation. A name
    // found via something more specific than a bare reference (a nested
    // `df["col"]`) is unaffected -- it never reaches the suppressed checks below.
    pub(super) suppressed_self_reference: Option<String>,
    pub(super) line_of: &'a dyn Fn(TextSize) -> usize,
}

impl UsageScan<'_> {
    pub(super) fn is_tainted(&self, expr: &Expr) -> bool {
        matches!(expr, Expr::Name(n) if self.tainted.contains(n.id.as_str()))
    }

    // `visit_expr` records a bare tainted `Name` reached generically (no more
    // specific handling applied) as an "Unknown" other-use -- see its own `Expr::Name`
    // arm. A pure alias (`out = df`, a walrus `y := df`) is NOT such a case: it's
    // already fully captured by `rebind_name`, so callers that are about to rebind
    // from this same expression call this instead of `visit_expr` directly, to avoid
    // double-reporting a plain rename as if it were an unrecognized use.
    pub(super) fn visit_unless_tainted(&mut self, expr: &Expr) {
        if !self.is_tainted(expr) {
            self.visit_expr(expr);
        }
    }

    // Like `is_tainted`, but returns the name itself -- callers that need to set
    // `suppressed_self_reference` to it (Subscript) or check it against an already-
    // suppressed name (Attribute, Call) use this instead.
    pub(super) fn tainted_name<'e>(&self, expr: &'e Expr) -> Option<&'e str> {
        match expr {
            Expr::Name(n) if self.tainted.contains(n.id.as_str()) => Some(n.id.as_str()),
            _ => None,
        }
    }

    pub(super) fn is_suppressed(&self, name: &str) -> bool {
        self.suppressed_self_reference.as_deref() == Some(name)
    }

    fn is_frame_column(&self, expr: &Expr) -> bool {
        matches!(expr, Expr::Subscript(s) if self.is_tainted(&s.value)
            && crate::ast_extract::extract_string_literal(&s.slice).is_some())
    }

    // A boolean row mask built from the tracked frame's own columns (`df["a"] > 0`,
    // `~df["a"].isin(keep)`, `df["a"].notna() & (df["b"] == 1)`). A bare name or a column of
    // another frame could be a column list, so neither is one.
    fn is_row_mask(&self, expr: &Expr) -> bool {
        match expr {
            Expr::UnaryOp(u) => {
                matches!(u.op, ast::UnaryOp::Invert) && self.is_row_mask(&u.operand)
            }
            Expr::BinOp(b) => {
                matches!(
                    b.op,
                    ast::Operator::BitAnd | ast::Operator::BitOr | ast::Operator::BitXor
                ) && self.is_row_mask(&b.left)
                    && self.is_row_mask(&b.right)
            }
            // A chained comparison asks for the truth of a Series, which pandas refuses.
            Expr::Compare(c) => {
                matches!(
                    &*c.ops,
                    [ast::CmpOp::Eq
                        | ast::CmpOp::NotEq
                        | ast::CmpOp::Lt
                        | ast::CmpOp::LtE
                        | ast::CmpOp::Gt
                        | ast::CmpOp::GtE]
                ) && std::iter::once(&*c.left)
                    .chain(c.comparators.iter())
                    .any(|e| self.is_frame_column(e))
            }
            Expr::Call(call) => {
                let Expr::Attribute(method) = &*call.func else {
                    return false;
                };
                let name = method.attr.as_str();
                (MASK_METHODS.contains(&name) && self.is_frame_column(&method.value))
                    || (STR_MASK_METHODS.contains(&name)
                        && matches!(&*method.value, Expr::Attribute(s)
                            if s.attr.as_str() == "str" && self.is_frame_column(&s.value)))
            }
            _ => false,
        }
    }

    // `df.loc[rows]` or `df.loc[rows, columns]` on a tracked name, `rows` being a row mask or
    // `:`.
    fn loc_selection<'e>(
        &self,
        sub: &'e ast::ExprSubscript,
    ) -> Option<(&'e Expr, Option<&'e Expr>)> {
        let Expr::Attribute(attr) = &*sub.value else {
            return None;
        };
        if attr.attr.as_str() != "loc" || !self.is_tainted(&attr.value) {
            return None;
        }
        let (rows, columns) = match &*sub.slice {
            Expr::Tuple(t) => match t.elts.as_slice() {
                [rows, columns] => (rows, Some(columns)),
                _ => return None,
            },
            rows => (rows, None),
        };
        (self.is_row_mask(rows) || is_all_rows(rows)).then_some((rows, columns))
    }

    // The row selector of an expression that keeps every column of the tracked frame and
    // only drops rows: `df[mask]`, `df[2:5]`, `df.loc[mask]`, `df.loc[mask, :]`.
    fn row_filter<'e>(&self, expr: &'e Expr) -> Option<&'e Expr> {
        let Expr::Subscript(sub) = expr else {
            return None;
        };
        if self.is_tainted(&sub.value) {
            return (self.is_row_mask(&sub.slice) || matches!(&*sub.slice, Expr::Slice(_)))
                .then_some(&*sub.slice);
        }
        match self.loc_selection(sub)? {
            (rows, None) => Some(rows),
            (rows, Some(columns)) if is_all_rows(columns) => Some(rows),
            _ => None,
        }
    }

    // `df[mask]["b"]`, `df.loc[mask, "b"]`, `df.loc[:, ["b", "c"]]`: reads of the mask's
    // columns and the selected ones. Returns whether `sub` was one.
    pub(super) fn visit_filtered_selection(&mut self, sub: &ast::ExprSubscript) -> bool {
        let (rows, columns) = match self.loc_selection(sub) {
            Some((rows, Some(columns))) if !is_all_rows(columns) => (rows, columns),
            _ => match self.row_filter(&sub.value) {
                Some(rows) => (rows, &*sub.slice),
                None => return false,
            },
        };
        let Some(selected) = crate::ast_extract::extract_string_list_or_single(columns) else {
            return false;
        };
        self.visit_expr(rows);
        let at = sub.range().start();
        for column in selected {
            self.record_column(at, column);
        }
        true
    }

    // The frame is passed as an argument. If the callee's use of that parameter is known
    // it counts as those column reads, made here; otherwise it is an unknown use.
    pub(super) fn passed_argument(
        &mut self,
        at: TextSize,
        passed: &str,
        via: Option<&str>,
        summary: Option<&ParamUsage>,
        callee: Option<&'static str>,
    ) {
        let (Some(summary), Some(via)) = (summary, via) else {
            let callee = if via.is_some() {
                Some(CALLEE_FORWARDS)
            } else {
                callee
            };
            self.record_other_via(at, passed.to_string(), Consumption::Unknown, callee);
            return;
        };
        let line = (self.line_of)(at);
        for (column, conditional) in &summary.accesses {
            // A column this scope created first isn't a source column.
            if self.written.contains(column) {
                continue;
            }
            self.usage.accesses.push(ColumnAccess {
                column: column.clone(),
                conditional: self.depth > 0 || *conditional,
                line,
                via: Some(via.to_string()),
            });
        }
    }

    // What the called function does with each of its parameters, and how to name it: a bare
    // function, or a method called on `self`.
    pub(super) fn summaries_for(&self, func: &Expr) -> Option<(String, Vec<ParamUsage>)> {
        match func {
            Expr::Name(f) => self
                .helpers
                .usage_of(f)
                .map(|summaries| (f.id.to_string(), summaries)),
            Expr::Attribute(a) if matches!(&*a.value, Expr::Name(n) if n.id.as_str() == "self") => {
                self.helpers
                    .method_usage_of(a.attr.as_str())
                    .map(|summaries| (format!("self.{}", a.attr), summaries))
            }
            _ => None,
        }
    }

    // Whether every tainted argument of `call` goes to a parameter whose use is known, so
    // the call's result is a new value rather than the tracked frame under another name
    // (a summarized function that handed its parameter back would not have a summary).
    fn call_is_fully_summarized(&self, call: &ast::ExprCall) -> bool {
        let Some((_, summaries)) = self.summaries_for(&call.func) else {
            return false;
        };
        let positional_ok = call
            .arguments
            .args
            .iter()
            .enumerate()
            .filter(|(_, arg)| self.is_tainted(arg))
            .all(|(i, _)| summaries.iter().any(|p| p.position == i));
        let keywords_ok = call
            .arguments
            .keywords
            .iter()
            .filter(|kw| self.is_tainted(&kw.value))
            .all(|kw| {
                kw.arg
                    .as_ref()
                    .is_some_and(|name| summaries.iter().any(|p| p.name == name.as_str()))
            });
        positional_ok && keywords_ok
    }

    // Whether `value` hands the tracked frame on: a plain alias or a call that returns it,
    // but not a call whose use of the frame is fully known.
    // `df.drop_duplicates()`, `df.dropna().copy()`: a new frame with the tracked frame's
    // columns, so reads of it are reads of the tracked frame. Only when no argument could
    // name a column (a count, a flag, `keep="first"`); `sort_values("a")` or
    // `drop_duplicates("id")` read columns this scan would lose by treating them as the
    // same frame.
    pub(super) fn derives_same_columns(&self, expr: &Expr) -> bool {
        match expr {
            Expr::Name(n) => self.tainted.contains(n.id.as_str()),
            Expr::Subscript(_) => self.row_filter(expr).is_some(),
            Expr::Call(call) => {
                let Expr::Attribute(attr) = &*call.func else {
                    return false;
                };
                // Without `drop=True` the old index becomes a column.
                if attr.attr.as_str() == "reset_index" {
                    return call.arguments.args.is_empty()
                        && matches!(&*call.arguments.keywords, [k]
                            if k.arg.as_ref().is_some_and(|a| a.as_str() == "drop")
                                && matches!(&k.value, Expr::BooleanLiteral(b) if b.value))
                        && self.derives_same_columns(&attr.value);
                }
                if let Some(added) = adding_columns_call(call) {
                    return added.is_some() && self.derives_same_columns(&attr.value);
                }
                if editing_columns_call(call).is_some()
                    || polars_filter_reads(call).is_some()
                    || row_op_reads(call).is_some()
                {
                    return self.derives_same_columns(&attr.value);
                }
                let literal = |e: &Expr| {
                    matches!(
                        e,
                        Expr::NumberLiteral(_) | Expr::BooleanLiteral(_) | Expr::NoneLiteral(_)
                    )
                };
                SAME_COLUMNS_METHODS.contains(&attr.attr.as_str())
                    && call.arguments.args.iter().all(literal)
                    && call.arguments.keywords.iter().all(|k| {
                        let name = k.arg.as_ref().map(|a| a.as_str());
                        name != Some("inplace")
                            && (literal(&k.value)
                                || matches!(
                                    (&k.value, name),
                                    (Expr::StringLiteral(_), Some("how" | "keep"))
                                ))
                    })
                    && self.derives_same_columns(&attr.value)
            }
            _ => false,
        }
    }

    // The reads and writes of the `assign`/`with_columns` calls in a derivation chain that
    // `derives_same_columns` accepted: their expressions read columns of the tracked frame
    // and the names they produce are not source columns.
    pub(super) fn record_derivation_effects(&mut self, expr: &Expr) {
        if let Some(rows) = self.row_filter(expr) {
            self.visit_expr(rows);
            return;
        }
        let Expr::Call(call) = expr else {
            return;
        };
        let Expr::Attribute(attr) = &*call.func else {
            return;
        };
        self.record_derivation_effects(&attr.value);
        if let Some(reads) = polars_filter_reads(call).or_else(|| row_op_reads(call)) {
            let at = call.range().start();
            for column in reads {
                self.record_column(at, column);
            }
        }
        if let Some((named, introduced)) = editing_columns_call(call) {
            let at = call.range().start();
            for column in named {
                self.record_column(at, column.clone());
                if !introduced.contains(&column) {
                    self.gone.insert(column);
                }
            }
            for column in &introduced {
                self.gone.remove(column);
            }
            self.written.extend(introduced);
        }
        if let Some(Some(added)) = adding_columns_call(call) {
            let at = call.range().start();
            if attr.attr.as_str() == "with_columns" {
                let reads: Vec<String> = call
                    .arguments
                    .args
                    .iter()
                    .chain(call.arguments.keywords.iter().map(|k| &k.value))
                    .flat_map(crate::ast_extract::collect_pl_col_names)
                    .collect();
                for column in reads {
                    self.record_column(at, column);
                }
            } else {
                for value in call
                    .arguments
                    .args
                    .iter()
                    .chain(call.arguments.keywords.iter().map(|k| &k.value))
                {
                    self.visit_expr(value);
                }
            }
            self.written.extend(added);
        }
    }

    pub(super) fn forwards(&self, value: &Expr) -> bool {
        self.derives_same_columns(value)
            || expr_forwards_tainted(&self.tainted, value)
                && !matches!(value, Expr::Call(call) if self.call_is_fully_summarized(call))
    }

    pub(super) fn taint(&mut self, name: &str) {
        self.tainted.insert(name.to_string());
        self.usage.ever_tainted.insert(name.to_string());
    }

    // A name being assigned either joins the tainted set (its value forwards an
    // already-tainted one) or, if it was PREVIOUSLY tainted, leaves it -- a name
    // reassigned to something unrelated no longer refers to the tracked origin, the
    // same "the old leg ends here" rule `Linter::end_stale_legs` applies elsewhere.
    // Without this, a name reassigned away from the origin would keep contributing
    // observations that aren't actually about it.
    //
    // Only true for a reassignment that always runs: under a branch it may or may
    // not happen, so a later read could still be of the original frame. Keep the
    // taint and record an unclassified use instead of dropping reads on the floor.
    pub(super) fn rebind_name(&mut self, at: TextSize, name: &str, forwards: bool) {
        if forwards {
            self.taint(name);
        } else if self.tainted.contains(name) {
            if self.depth > 0 {
                self.record_other(
                    at,
                    format!("`{name}` conditionally reassigned"),
                    Consumption::Unknown,
                );
            } else {
                self.tainted.remove(name);
            }
        }
    }
}

use super::*;

impl Linter {
    // A function that returns the frame is as wide as its callers need. When every call to
    // it in the project is seen and each only reads named columns of the result, the
    // `returned` uses stand for those reads. Callers outside the project, or reaching the
    // function dynamically, can't be seen, so the suggestion carries a note.
    fn see_through_returns(&self, scope: &UsageScope, usage: &mut column_usage::ColumnUsage) {
        let (Some(name), Some(index)) = (scope.function_name, self.project_index.as_ref()) else {
            return;
        };
        let Some(callers) = index.call_usage.get(name) else {
            return;
        };
        let blockers: Vec<&column_usage::OtherUse> = usage
            .other_uses
            .iter()
            .filter(|u| column_usage::blocks_narrowing(u))
            .collect();
        if blockers.is_empty() || !blockers.iter().all(|u| u.description == "returned") {
            return;
        }
        let line = blockers[0].line;
        usage.other_uses.retain(|u| u.description != "returned");
        let via = format!("callers of `{name}` ({} calls)", callers.calls);
        for column in &callers.columns {
            usage.accesses.push(column_usage::ColumnAccess {
                column: column.clone(),
                conditional: false,
                line,
                via: Some(via.clone()),
            });
        }
    }

    // If `stmt` is the origin of an untyped site, records its downstream usage (and, when
    // suggestions are on, the outcome of trying to make one).
    pub(super) fn scan_origin(
        &self,
        scope: &UsageScope,
        stmt: &Stmt,
        rest: &[Stmt],
        tails: &[&[Stmt]],
        in_loop: bool,
        results: &mut SiteScanResults,
    ) {
        let assigned = match stmt {
            Stmt::Assign(assign) if matches!(assign.targets.as_slice(), [Expr::Name(_)]) => {
                Some((assign.range().start(), Some(&*assign.value), None))
            }
            Stmt::AnnAssign(ann) if matches!(&*ann.target, Expr::Name(_)) => Some((
                ann.range().start(),
                ann.value.as_deref(),
                Some(&*ann.annotation),
            )),
            _ => None,
        };
        let Some((start, value, annotation)) = assigned else {
            return;
        };
        let pos = self.source_location(start);
        let Some(&var) = scope.by_position.get(&pos) else {
            return;
        };
        // Other methods read a frame stored on `self`; this scope can't see them.
        if let ControlFlow::Break(_) = self.note_other_method_reads(in_loop, results, pos, var) {
            return;
        }
        let usage = self.scan_origin_usage(scope, rest, tails, var);
        if self.collect_suggestions {
            let next = rest
                .first()
                .or_else(|| tails.iter().find_map(|t| t.first()));
            let outcome = if next.is_some_and(|n| suggest::is_column_selection_of(n, var)) {
                Err("it is already followed by a column selection".to_string())
            } else if usage
                .ever_tainted
                .iter()
                .any(|alias| column_usage::declares_global_or_nonlocal(scope.body, alias))
            {
                Err(format!(
                    "`{var}` (or a name it was assigned to) is declared `global` or `nonlocal` here, so other \
                     code can read it"
                ))
            } else if scope
                .function_name
                .is_some_and(|name| name == "test" || name.starts_with("test_"))
            {
                Err(
                    "it is inside a test, which should see the whole frame rather than a \
                     narrowed copy"
                        .to_string(),
                )
            } else {
                suggest::suggest_selection(
                    &suggest::Origin {
                        libraries: scope.libraries,
                        annotation,
                        source: &self.source,
                        display_path: &self.suggest_display_path,
                        value,
                        var,
                        line: pos.0,
                        col: pos.1,
                        stmt_start: stmt.range().start().to_usize(),
                        stmt_end: stmt.range().end().to_usize(),
                        function_local: scope.in_function,
                        receivers: &scope.receivers,
                        known_columns: &self.known_columns,
                        scope_body: scope.body,
                    },
                    &usage,
                )
            };
            results.outcomes.push((pos, outcome));
        }
        if !usage.accesses.is_empty() || !usage.other_uses.is_empty() {
            results.usage.insert(pos, usage);
        }
    }

    fn note_other_method_reads(
        &self,
        in_loop: bool,
        results: &mut SiteScanResults,
        pos: (usize, usize),
        var: &str,
    ) -> ControlFlow<()> {
        if var.starts_with(crate::self_frames::SELF_PREFIX) || var.contains('[') {
            return ControlFlow::Break(());
        }
        if in_loop {
            if self.collect_suggestions {
                results.outcomes.push((
                    pos,
                    Err(
                        "its origin is inside a loop, where the frame can be read again on a \
                         later pass before it is reassigned"
                            .to_string(),
                    ),
                ));
            }
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    }

    fn scan_origin_usage(
        &self,
        scope: &UsageScope<'_>,
        rest: &[Stmt],
        tails: &[&[Stmt]],
        var: &str,
    ) -> column_usage::ColumnUsage {
        let line_of = |offset: ruff_text_size::TextSize| self.source_location(offset).0;
        let downstream = std::iter::once(rest)
            .chain(tails.iter().copied())
            .flat_map(|stmts| stmts.iter());
        let mut usage = column_usage::scan_usage_with(var, downstream, &line_of, scope.helpers);
        // A closure can read columns the scan never sees, wherever in the scope it is
        // defined (a function above the origin still runs later), so ask about the whole
        // scope.
        if let Some(offset) =
            column_usage::first_nested_scope_mention(scope.body, &usage.ever_tainted)
        {
            usage.other_uses.push(column_usage::OtherUse {
                description:
                    "also mentioned inside a nested function, class, lambda or generator expression"
                        .to_string(),
                conditional: false,
                consumes: column_usage::Consumption::Unknown,
                line: line_of(offset),
                callee: None,
            });
        }
        self.see_through_returns(scope, &mut usage);
        usage
    }

    // Turns per-site outcomes into `suggestions`/`withheld`, and withholds every untyped
    // site the scan never reached (nested in control flow, a class body, or unpacked)
    // so each one is accounted for.
    pub(super) fn settle_suggestions(
        &mut self,
        outcomes: Vec<((usize, usize), Result<suggest::Suggestion, String>)>,
    ) {
        let var_at: HashMap<(usize, usize), String> = self
            .untyped_sites
            .iter()
            .map(|site| ((site.line, site.col), site.var.clone()))
            .collect();
        let phrase_at: HashMap<(usize, usize), String> = self
            .untyped_sites
            .iter()
            .map(|site| ((site.line, site.col), site.phrase.clone()))
            .collect();
        let mut reached = std::collections::HashSet::new();
        for (pos, outcome) in outcomes {
            reached.insert(pos);
            match outcome {
                Ok(mut suggestion) => {
                    suggestion.phrase = phrase_at.get(&pos).cloned().unwrap_or_default();
                    self.suggestions.push(suggestion);
                }
                Err(reason) => self.withheld.push(suggest::Withheld {
                    phrase: phrase_at.get(&pos).cloned().unwrap_or_default(),
                    line: pos.0,
                    col: pos.1,
                    var: var_at.get(&pos).cloned().unwrap_or_default(),
                    reason,
                    cell: None,
                }),
            }
        }
        // A later leg of a frame (`items["a"] = ...` on a frame already reported) is not a
        // second frame; say it once, under the origin's own reason.
        let mut said: std::collections::HashSet<&str> = reached
            .iter()
            .filter_map(|pos| var_at.get(pos).map(String::as_str))
            .collect();
        for site in &self.untyped_sites {
            if !reached.contains(&(site.line, site.col)) && said.insert(site.var.as_str()) {
                self.withheld.push(suggest::Withheld {
                    phrase: site.phrase.clone(),
                    line: site.line,
                    col: site.col,
                    var: site.var.clone(),
                    reason:
                        "its origin isn't a plain `name = ...` assignment in a function, method or \
                             module (it is a class attribute, unpacked, or an attribute or \
                             subscript target)"
                            .to_string(),
                    cell: None,
                });
            }
        }
        self.suggestions.sort_by_key(|s| (s.line, s.col));
        self.withheld.sort_by_key(|w| (w.line, w.col));
    }
}

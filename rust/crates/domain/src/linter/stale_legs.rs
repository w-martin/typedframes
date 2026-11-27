use super::*;

impl Linter {
    // Visit every statement in a nested body (function/class/if/for/while/with/try) in
    // order. Pulled out as its own method since every one of those statement kinds
    // needs exactly this loop over one or more of its own `Vec<Stmt>` fields.
    pub(super) fn visit_body(&mut self, body: &[Stmt], errors: &mut Vec<LintError>) {
        self.block_depth += 1;
        for stmt in body {
            self.visit_stmt(stmt, errors);
        }
        self.merge_shadowed_frames();
        self.block_depth -= 1;
    }

    // Leaving a block that rebound an outer open frame: only what both values lack is
    // certainly gone, and the frame is back at its outer depth.
    pub(super) fn merge_shadowed_frames(&mut self) {
        let depth = self.block_depth;
        let (ending, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.shadowed_frames)
            .into_iter()
            .partition(|(d, _, _)| *d == depth);
        self.shadowed_frames = kept;
        for (_, name, mut outer) in ending {
            if let Some(inner) = self.open_frames.get(&name) {
                outer.absent.retain(|c, _| inner.absent.contains_key(c));
                outer.written.extend(inner.written.iter().cloned());
                self.open_frames.insert(name, outer);
            }
        }
    }

    pub(super) fn record_leg_event(
        &mut self,
        var: &str,
        line: usize,
        col: usize,
        outcome: &str,
        reason: String,
    ) {
        // One row per name and statement: the first reason recorded is the specific one.
        if self
            .leg_events
            .iter()
            .any(|e| e.line == line && e.col == col && e.var == var && e.outcome == outcome)
        {
            return;
        }
        self.leg_events.push(LegEvent {
            line,
            col,
            var: var.to_string(),
            outcome: outcome.to_string(),
            reason,
        });
    }

    // Every plain name the statement rebinds that already carries a tracked schema,
    // with that binding and whether the name is a direct target (`df = ...`) rather
    // than one unpacked from a tuple/list. Captured BEFORE the statement is walked so
    // `end_stale_legs` can tell afterwards whether the dispatch rebound it.
    fn snapshot_rebinding_targets(&self, stmt: &Stmt) -> Vec<(String, (String, usize), bool)> {
        let mut names = Vec::new();
        match stmt {
            Stmt::Assign(assign) => {
                for target in &assign.targets {
                    collect(target, matches!(target, Expr::Name(_)), &mut names);
                }
            }
            Stmt::AnnAssign(ann) if ann.value.is_some() => collect(&ann.target, true, &mut names),
            _ => {}
        }
        names
            .into_iter()
            .filter_map(|(name, direct)| {
                self.variables
                    .get(name)
                    .map(|binding| (name.to_string(), binding.clone(), direct))
            })
            .collect()
    }

    // A tracked name reassigned by a statement the dispatch did not model keeps its OLD
    // schema, which is either wrong (the name now holds something else) or only right
    // by luck. Ends that stale leg:
    //
    // - same-name, schema-preserving (`df = df.copy()`, `df = df.head()`): nothing to do.
    // - same-name, derived via an op the checker does not model (`df = clean(df)`,
    //   `df = df.groupby(...).agg(...)`): the frame's columns are now unknown, so it
    //   becomes an unresolved leg -- counted once, when a resolved leg is lost. An
    //   already-unresolved leg passing through another unknown op stays one leg.
    // - anything else (a Series/scalar result, an unrelated source, tuple unpacking, a
    //   call to a helper defined later in the file): the name no longer carries that
    //   frame, so it goes untracked. No claim is made about it.
    pub(super) fn end_stale_legs(
        &mut self,
        stmt: &Stmt,
        rebinding: Vec<(String, (String, usize), bool)>,
    ) {
        let (value, start) = match stmt {
            Stmt::Assign(assign) => (Some(&*assign.value), assign.range().start()),
            Stmt::AnnAssign(ann) => (ann.value.as_deref(), ann.range().start()),
            _ => return,
        };
        let (current_line, current_col) = self.source_location(start);
        let what = value.map_or_else(|| "this statement".to_string(), describe_rhs);
        for (name, old, direct) in rebinding {
            if self.variables.get(&name) != Some(&old) {
                continue;
            }
            if let Some(v) = value.filter(|v| {
                direct
                    && self
                        .helper_call(v)
                        .is_some_and(|(arg, summary)| arg == name && summary.returned.is_some())
            }) {
                self.apply_helper_call(v, std::slice::from_ref(&name), current_line, current_col);
                continue;
            }
            let derivation = if direct {
                value.and_then(|v| {
                    let variables = &self.variables;
                    classify_rhs(v, &|n| variables.contains_key(n))
                })
            } else {
                None
            };
            match derivation {
                Some(d) if d.root == name && d.kind == Kind::Preserves => {}
                // Rows stacked from a frame the checker cannot see: the known columns are
                // still there, but nothing says what else came with them, so this is not an
                // unresolved frame (whose every read would be flagged) but an untracked one.
                Some(d)
                    if d.root == name
                        && d.kind == Kind::Derived
                        && value.is_some_and(is_concat_call) =>
                {
                    self.variables.remove(&name);
                    self.record_leg_event(
                        &name,
                        current_line,
                        current_col,
                        "untracked",
                        format!("{what} joins a frame the checker does not track"),
                    );
                }
                Some(d) if d.root == name && d.kind == Kind::Derived => {
                    if let ControlFlow::Break(_) =
                        self.end_stale_leg(value, current_line, current_col, &what, &name, old)
                    {
                        continue;
                    }
                }
                other => {
                    let reason = match other {
                        _ if !direct => "rebound by tuple unpacking".to_string(),
                        Some(d) if d.kind == Kind::NotAFrame => {
                            format!("reassigned to a Series or scalar via {what}")
                        }
                        _ => format!("reassigned to a value the checker does not track ({what})"),
                    };
                    self.variables.remove(&name);
                    self.record_leg_event(&name, current_line, current_col, "untracked", reason);
                }
            }
        }
    }

    fn end_stale_leg(
        &mut self,
        value: Option<&Expr>,
        current_line: usize,
        current_col: usize,
        what: &String,
        name: &String,
        old: (String, usize),
    ) -> ControlFlow<()> {
        if value.is_some_and(|v| self.calls_later_defined_function(v)) {
            self.variables.remove(name);
            self.record_leg_event(
                name,
                current_line,
                current_col,
                "untracked",
                format!(
                    "{what} is defined later in this file, so its result cannot be inferred yet"
                ),
            );
        } else if !self.schema_is_unresolved(&old.0) {
            let columns = value.and_then(|v| self.returned_expr_columns(v));
            if let Some(columns) = columns {
                let schema = self.count_mutation_leg(
                    Some(&old.0),
                    Some(columns),
                    name,
                    current_line,
                    current_col,
                );
                if let Some(v) = value {
                    self.carry_index(v, &schema);
                }
                self.variables.insert(name.clone(), (schema, current_line));
                return ControlFlow::Break(());
            }
            let schema =
                self.count_mutation_leg(Some(&old.0), None, name, current_line, current_col);
            self.variables.insert(name.clone(), (schema, current_line));
            self.record_leg_event(
                name,
                current_line,
                current_col,
                "unresolved",
                format!("{what} is not modelled, so the columns are unknown from here"),
            );
        }
        ControlFlow::Continue(())
    }

    // Wraps `visit_stmt_inner` so that, for any assignment, a tracked name the dispatch
    // left untouched can be checked afterwards -- see `end_stale_legs`.
    pub(super) fn visit_stmt(&mut self, stmt: &Stmt, errors: &mut Vec<LintError>) {
        let unwrapped = match stmt {
            Stmt::Assign(assign) => self.unwrap_forwarder(assign).map(Stmt::Assign),
            _ => None,
        };
        if let (None, Stmt::Assign(assign)) = (&unwrapped, stmt) {
            self.note_unfollowed_forwarder(assign);
        }
        let stmt = unwrapped.as_ref().unwrap_or(stmt);
        self.untrack_rebound_names(stmt);
        self.stmt_counter += 1;
        let sites_before = self.untyped_sites.len();
        let rebinding = self.snapshot_rebinding_targets(stmt);
        if let (Stmt::Assign(assign), false) = (stmt, rebinding.is_empty()) {
            self.visit_expr(&assign.value, errors);
            self.value_checked_early = true;
        }
        if let Stmt::Assign(assign) = stmt {
            let (line, col) = self.source_location(assign.range().start());
            self.apply_columns_assignment(assign, line, col);
        }
        let new_name = self.new_name_binding(stmt);
        self.visit_stmt_inner(stmt, errors);
        self.value_checked_early = false;
        self.track_instance_binding(stmt);
        if !rebinding.is_empty() {
            self.end_stale_legs(stmt, rebinding);
        }
        if let Some((name, before)) = new_name {
            self.bind_derived_new_name(stmt, &name, before);
        }
        self.bind_tuple_result(stmt);
        self.update_open_frames(stmt, sites_before, errors);
        self.note_index_names(stmt);
        self.forget_self_frames(stmt);
    }

    // A frame given a named index (constructor `index=`, `df.index.name = ..`, an in-place
    // `rename_axis`/`set_index`) may take that name where a column name is expected.
    fn note_index_names(&mut self, stmt: &Stmt) {
        let (frame, levels) = match stmt {
            Stmt::Assign(assign) => match (assign.targets.as_slice(), &*assign.value) {
                ([Expr::Name(target)], Expr::Call(call)) => {
                    let Some(index) = constructor_index(call) else {
                        return;
                    };
                    (target.id.as_str(), index_name_of(index))
                }
                ([Expr::Attribute(attr)], value) => match index_owner(&attr.value) {
                    Some(owner) if matches!(attr.attr.as_str(), "name" | "names") => {
                        (owner, ast_extract::extract_string_list_or_single(value))
                    }
                    _ => match (attr.attr.as_str(), &*attr.value) {
                        ("index", Expr::Name(owner)) => (owner.id.as_str(), index_name_of(value)),
                        _ => return,
                    },
                },
                _ => return,
            },
            Stmt::Expr(expr) => {
                let Expr::Call(call) = &*expr.value else {
                    return;
                };
                let Expr::Attribute(attr) = &*call.func else {
                    return;
                };
                let in_place = keyword(call, "inplace")
                    .is_some_and(|k| !matches!(&k.value, Expr::BooleanLiteral(b) if !b.value));
                if !in_place {
                    return;
                }
                let owner = match (attr.attr.as_str(), &*attr.value) {
                    ("rename_axis" | "set_index" | "set_axis", Expr::Name(n)) => n.id.as_str(),
                    ("set_names" | "rename", value) => match index_owner(value) {
                        Some(owner) => owner,
                        None => return,
                    },
                    _ => return,
                };
                (owner, None)
            }
            _ => return,
        };
        if levels.as_ref().is_some_and(Vec::is_empty) {
            return;
        }
        let Some((schema, _)) = self.variables.get(frame) else {
            return;
        };
        let schema = schema.clone();
        match levels {
            Some(levels) => {
                self.unknown_index.remove(&schema);
                self.schema_index.insert(schema, levels);
            }
            None => {
                self.unknown_index.insert(schema);
            }
        }
    }

    // A call that can reach `self` may change any frame stored on it, so those frames go back
    // to their method-entry state: open, no facts. Code can reach `self` without naming it
    // (`super().m()`, a closure, `c = self.child; c.fill()`), so every call except frame
    // methods, pandas/polars functions and plain builtins counts, as does aliasing `self` or
    // storing to another object's attribute.
    fn forget_self_frames(&mut self, stmt: &Stmt) {
        let prefix = crate::self_frames::SELF_PREFIX;
        let foreign_store = match self.forget_open_self_frames(stmt, prefix) {
            Some(value) => value,
            None => return,
        };
        let mut reach = ReachesSelf {
            linter: self,
            reaches: foreign_store,
            call: false,
            self_name: false,
        };
        for expr in own_expressions(stmt) {
            reach.visit_expr(expr);
        }
        if !(reach.reaches || (reach.call && reach.self_name)) {
            return;
        }
        self.self_frame_names(prefix);
    }

    fn forget_open_self_frames(&mut self, stmt: &Stmt, prefix: &str) -> Option<bool> {
        if !self.open_frames.keys().any(|n| n.starts_with(prefix))
            && !self.variables.keys().any(|n| n.starts_with(prefix))
        {
            return None;
        }
        let foreign_store = match stmt {
            Stmt::Assign(s) => s.targets.iter().any(stores_to_foreign_attribute),
            Stmt::AugAssign(s) => stores_to_foreign_attribute(&s.target),
            Stmt::AnnAssign(s) => stores_to_foreign_attribute(&s.target),
            _ => false,
        };
        Some(foreign_store)
    }

    fn self_frame_names(&mut self, prefix: &str) {
        let names: Vec<String> = self
            .open_frames
            .keys()
            .chain(self.variables.keys())
            .filter(|n| n.starts_with(prefix))
            .cloned()
            .collect();
        for name in names {
            self.variables.remove(&name);
            self.open_frames.insert(
                name,
                OpenFrame {
                    depth: self.block_depth,
                    ..OpenFrame::default()
                },
            );
        }
        self.open_reads
            .borrow_mut()
            .retain(|(frame, _), _| !frame.starts_with(prefix));
    }
}

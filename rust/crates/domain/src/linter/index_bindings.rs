use super::*;

impl Linter {
    // `a, b = load()` where `load` returns a tuple of frames whose columns it knows.
    pub(super) fn bind_tuple_result(&mut self, stmt: &Stmt) {
        let Stmt::Assign(assign) = stmt else {
            return;
        };
        let ([Expr::Tuple(targets)], Expr::Call(call)) =
            (assign.targets.as_slice(), &*assign.value)
        else {
            return;
        };
        let Expr::Name(callee) = &*call.func else {
            return;
        };
        let Some(schemas) = self.tuple_functions.get(callee.id.as_str()) else {
            return;
        };
        if schemas.len() != targets.elts.len() {
            return;
        }
        let line = self.source_location(assign.range().start()).0;
        let bound: Vec<(String, String)> = targets
            .elts
            .iter()
            .zip(schemas)
            .filter_map(|(target, schema)| match (target, schema) {
                (Expr::Name(n), Some(schema)) => Some((n.id.to_string(), schema.clone())),
                _ => None,
            })
            .collect();
        for (name, schema) in bound {
            self.variables.insert(name, (schema, line));
        }
    }

    // `b = <chain on a tracked frame>`: the name being bound and what it was bound to before,
    // for `bind_derived_new_name`. Not a rebinding of the chain's own root, which
    // `end_stale_legs` already follows.
    pub(super) fn new_name_binding(
        &self,
        stmt: &Stmt,
    ) -> Option<(String, Option<(String, usize)>)> {
        let Stmt::Assign(assign) = stmt else {
            return None;
        };
        let [Expr::Name(target)] = assign.targets.as_slice() else {
            return None;
        };
        if !matches!(&*assign.value, Expr::Call(_) | Expr::Subscript(_)) {
            return None;
        }
        let name = target.id.as_str();
        let variables = &self.variables;
        let own_root = classify_rhs(&assign.value, &|n| variables.contains_key(n))
            .is_some_and(|d| d.root == name);
        (!own_root).then(|| (name.to_string(), self.variables.get(name).cloned()))
    }

    // The dispatch above models a few methods by name; any other chain whose columns follow
    // from literal edits (`.copy()`, `.astype()`, `.loc[mask]`, `.set_index()`, ...) is
    // resolved here, so deriving a frame under a new name does not lose its columns.
    pub(super) fn bind_derived_new_name(
        &mut self,
        stmt: &Stmt,
        name: &str,
        before: Option<(String, usize)>,
    ) {
        let Stmt::Assign(assign) = stmt else {
            return;
        };
        if self
            .variables
            .get(name)
            .is_some_and(|now| Some(now) != before.as_ref())
        {
            return;
        }
        let (line, col) = self.source_location(assign.range().start());
        let Some(columns) = self.returned_expr_columns(&assign.value) else {
            let variables = &self.variables;
            let derived = classify_rhs(&assign.value, &|n| variables.contains_key(n));
            if derived.is_some_and(|d| d.root != name && d.kind == Kind::Derived) {
                self.note_unmodelled_derivation(name, &assign.value, line, col);
            }
            return;
        };
        let schema = self.make_inferred_schema(columns, name, line);
        self.carry_index(&assign.value, &schema);
        self.variables.insert(name.to_string(), (schema, line));
    }

    // The index a derived frame has: a new one from `index_levels`, else its source's
    // (`df.copy()`, `df[mask]`) unless the chain turns the index into columns.
    pub(super) fn carry_index(&mut self, value: &Expr, schema: &str) {
        if let Some(levels) = self.index_levels(value) {
            self.schema_index.insert(schema.to_string(), levels);
            return;
        }
        let mut expr = value;
        let root = loop {
            expr = match expr {
                Expr::Call(call) => match &*call.func {
                    Expr::Attribute(a)
                        if !matches!(a.attr.as_str(), "reset_index" | "to_frame" | "set_index") =>
                    {
                        &a.value
                    }
                    _ => return,
                },
                Expr::Attribute(a) if a.attr.as_str() != "T" => &a.value,
                Expr::Subscript(sub) => &sub.value,
                Expr::Name(n) => break n.id.as_str(),
                _ => return,
            };
        };
        let Some((source, _)) = self.variables.get(root) else {
            return;
        };
        if source == schema {
            return;
        }
        let source = source.clone();
        if let Some(levels) = self.schema_index.get(&source).cloned() {
            self.schema_index.insert(schema.to_string(), levels);
        } else if self.unknown_index.contains(&source) {
            self.unknown_index.insert(schema.to_string());
        }
    }

    // The named index a `groupby(..).agg(out=..)` or `pivot_table(index=..)` result carries.
    pub(super) fn index_levels(&self, expr: &Expr) -> Option<Vec<String>> {
        let Expr::Call(call) = expr else {
            return None;
        };
        let Expr::Attribute(attr) = &*call.func else {
            return None;
        };
        match attr.attr.as_str() {
            "pivot_table" => {
                ast_extract::extract_string_list_or_single(&keyword(call, "index")?.value)
            }
            _ if Self::groupby_selection_aggregate(call, false).is_some() => {
                let Expr::Subscript(selection) = &*attr.value else {
                    return None;
                };
                let Expr::Call(grouped) = &*selection.value else {
                    return None;
                };
                if keyword(grouped, "as_index").is_some() {
                    return None;
                }
                let by = grouped
                    .arguments
                    .args
                    .first()
                    .or_else(|| keyword(grouped, "by").map(|k| &k.value))?;
                ast_extract::extract_string_list_or_single(by)
            }
            "set_index" if keyword(call, "inplace").is_none() => {
                let keys = call
                    .arguments
                    .args
                    .first()
                    .or_else(|| keyword(call, "keys").map(|k| &k.value))?;
                ast_extract::extract_string_list_or_single(keys)
            }
            "agg" | "aggregate" if Self::series_agg_list(call, false).is_some() => {
                Self::series_groupby_keys(&attr.value)
            }
            "to_frame" => self.series_index(&attr.value).filter(|l| !l.is_empty()),
            "agg" | "aggregate" => {
                let Expr::Call(grouped) = &*attr.value else {
                    return None;
                };
                if keyword(grouped, "as_index").is_some() {
                    return None;
                }
                let with_keys = Self::named_agg_columns(call, true)?;
                let outputs = Self::named_agg_columns(call, false)?;
                Some(with_keys[..with_keys.len() - outputs.len()].to_vec())
            }
            _ => None,
        }
    }
}

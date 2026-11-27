use super::*;

impl Linter {
    // `df.groupby(keys).agg(out=("col", "fn"), ..)`: the outputs, with the keys first once
    // they are columns (`as_index=False`, or after `reset_index()`) rather than the index.
    pub(super) fn named_agg_columns(
        call: &ast::ExprCall,
        reset_index: bool,
    ) -> Option<Vec<String>> {
        let Expr::Attribute(attr) = &*call.func else {
            return None;
        };
        let Expr::Call(grouped) = &*attr.value else {
            return None;
        };
        let Expr::Attribute(group_attr) = &*grouped.func else {
            return None;
        };
        if group_attr.attr.as_str() != "groupby" {
            return None;
        }
        let mut outputs = Vec::new();
        match &*call.arguments.args {
            [] => {}
            // `agg({"a": "sum", "b": "mean"})`: one function per column. A list of
            // functions would make the columns a MultiIndex.
            [Expr::Dict(spec)] if call.arguments.keywords.is_empty() => {
                for item in &spec.items {
                    let name = ast_extract::extract_string_literal(item.key.as_ref()?)?;
                    ast_extract::extract_string_literal(&item.value)?;
                    outputs.push(name.to_string());
                }
            }
            _ => return None,
        }
        for kw in &call.arguments.keywords {
            let Expr::Tuple(pair) = &kw.value else {
                return None;
            };
            if pair.elts.len() != 2 {
                return None;
            }
            outputs.push(kw.arg.as_ref()?.to_string());
        }
        if outputs.is_empty() {
            return None;
        }
        let mut keys_are_columns = reset_index;
        for kw in &grouped.arguments.keywords {
            match kw.arg.as_ref().map(|a| a.as_str()) {
                Some("as_index") => match &kw.value {
                    Expr::BooleanLiteral(b) => keys_are_columns = keys_are_columns || !b.value,
                    _ => return None,
                },
                Some("sort" | "dropna" | "observed" | "group_keys") => {}
                Some("by") => {}
                _ => return None,
            }
        }
        if !keys_are_columns {
            return Some(outputs);
        }
        let by = grouped
            .arguments
            .args
            .first()
            .or_else(|| keyword(grouped, "by").map(|k| &k.value))?;
        let mut columns = ast_extract::extract_string_list_or_single(by)?;
        columns.extend(outputs);
        Some(columns)
    }

    // `frame.groupby(keys)[[cols]].mean()`: the group keys, the selected columns, and whether the
    // keys are columns of the result (`as_index=False`, or after `reset_index()`).
    pub(super) fn groupby_selection_aggregate(
        call: &ast::ExprCall,
        reset_index: bool,
    ) -> Option<Vec<String>> {
        let Expr::Attribute(attr) = &*call.func else {
            return None;
        };
        if !GROUPBY_SELECTION_AGGREGATE_REDUCTIONS.contains(&attr.attr.as_str()) {
            return None;
        }
        let Expr::Subscript(selection) = &*attr.value else {
            return None;
        };
        let Expr::Call(grouped) = &*selection.value else {
            return None;
        };
        let Expr::Attribute(group_attr) = &*grouped.func else {
            return None;
        };
        if group_attr.attr.as_str() != "groupby" {
            return None;
        }
        // A single name selects a Series, which has no columns to name.
        let selected = match &*selection.slice {
            list @ (Expr::List(_) | Expr::Tuple(_)) => ast_extract::extract_string_list(list)?,
            _ if reset_index => {
                vec![ast_extract::extract_string_literal(&selection.slice)?.to_string()]
            }
            _ => return None,
        };
        let mut keys_are_columns = reset_index;
        for kw in &grouped.arguments.keywords {
            match kw.arg.as_ref().map(|a| a.as_str()) {
                Some("as_index") => match &kw.value {
                    Expr::BooleanLiteral(b) => keys_are_columns = keys_are_columns || !b.value,
                    _ => return None,
                },
                Some("sort" | "dropna" | "observed" | "group_keys" | "by") => {}
                _ => return None,
            }
        }
        if !keys_are_columns {
            return Some(selected);
        }
        let by = grouped
            .arguments
            .args
            .first()
            .or_else(|| keyword(grouped, "by").map(|k| &k.value))?;
        let mut columns = ast_extract::extract_string_list_or_single(by)?;
        for column in selected {
            if !columns.contains(&column) {
                columns.push(column);
            }
        }
        Some(columns)
    }

    // `frame.groupby(keys)["v"]` with the keys as the index: the keys.
    pub(super) fn series_groupby_keys(expr: &Expr) -> Option<Vec<String>> {
        let grouped = match expr {
            Expr::Subscript(selection) => {
                ast_extract::extract_string_literal(&selection.slice)?;
                match &*selection.value {
                    Expr::Call(grouped) => grouped,
                    _ => return None,
                }
            }
            Expr::Call(grouped) => grouped,
            _ => return None,
        };
        let Expr::Attribute(group_attr) = &*grouped.func else {
            return None;
        };
        if group_attr.attr.as_str() != "groupby" {
            return None;
        }
        for kw in &grouped.arguments.keywords {
            match kw.arg.as_ref().map(|a| a.as_str()) {
                Some("sort" | "dropna" | "observed" | "group_keys" | "by") => {}
                _ => return None,
            }
        }
        let by = grouped
            .arguments
            .args
            .first()
            .or_else(|| keyword(grouped, "by").map(|k| &k.value))?;
        ast_extract::extract_string_list_or_single(by)
    }

    // A Series per group: `groupby(keys)["v"].<reduction>()` or `groupby(keys).size()`.
    pub(super) fn series_groupby_reduction_keys(call: &ast::ExprCall) -> Option<Vec<String>> {
        let Expr::Attribute(attr) = &*call.func else {
            return None;
        };
        if !call.arguments.args.is_empty() {
            return None;
        }
        match (attr.attr.as_str(), &*attr.value) {
            ("size", Expr::Call(_)) => Self::series_groupby_keys(&attr.value),
            (m, Expr::Subscript(_)) if REDUCTIONS.contains(&m) => {
                Self::series_groupby_keys(&attr.value)
            }
            _ => None,
        }
    }

    // `groupby(keys)["v"].agg(["mean", "sum"])`: one flat column per named function, with the
    // keys as columns once reset. On a frame (not one Series) the columns would be a MultiIndex.
    pub(super) fn series_agg_list(call: &ast::ExprCall, reset_index: bool) -> Option<Vec<String>> {
        let Expr::Attribute(attr) = &*call.func else {
            return None;
        };
        if !matches!(attr.attr.as_str(), "agg" | "aggregate") || !call.arguments.keywords.is_empty()
        {
            return None;
        }
        let [Expr::List(funcs)] = &*call.arguments.args else {
            return None;
        };
        if !matches!(&*attr.value, Expr::Subscript(_)) {
            return None;
        }
        let keys = Self::series_groupby_keys(&attr.value)?;
        let outputs = funcs
            .elts
            .iter()
            .map(|f| ast_extract::extract_string_literal(f).map(str::to_string))
            .collect::<Option<Vec<_>>>()?;
        if outputs.is_empty() {
            return None;
        }
        if !reset_index {
            return Some(outputs);
        }
        let mut columns = keys;
        columns.extend(outputs);
        Some(columns)
    }

    // The name a Series expression carries: `<frame-like>["v"]`, kept through a groupby or
    // resample reduction and through elementwise methods.
    pub(super) fn series_name(&self, expr: &Expr) -> Option<String> {
        match expr {
            Expr::Subscript(sub) => {
                // Only a frame or a grouping names its Series after the key; a dict does not.
                let frame_like = match &*sub.value {
                    Expr::Name(n) => self
                        .variables
                        .get(n.id.as_str())
                        .is_some_and(|(schema, _)| !self.schema_is_unresolved(schema)),
                    Expr::Call(c) => matches!(&*c.func, Expr::Attribute(a) if matches!(
                        a.attr.as_str(),
                        "groupby" | "resample" | "rolling" | "expanding"
                    )),
                    _ => false,
                };
                if !frame_like {
                    return None;
                }
                Some(ast_extract::extract_string_literal(&sub.slice)?.to_string())
            }
            Expr::Call(call) => match &*call.func {
                Expr::Attribute(a) if KEEPS_NAME.contains(&a.attr.as_str()) => {
                    self.series_name(&a.value)
                }
                _ => None,
            },
            _ => None,
        }
    }

    // The named index levels of a Series `series_name` names, when they are known: empty for
    // a tracked frame without one.
    pub(super) fn series_index(&self, expr: &Expr) -> Option<Vec<String>> {
        match expr {
            Expr::Subscript(sub) => match &*sub.value {
                Expr::Name(n) => {
                    let (schema, _) = self.variables.get(n.id.as_str())?;
                    Some(self.schema_index.get(schema).cloned().unwrap_or_default())
                }
                Expr::Call(c) => {
                    let Expr::Attribute(a) = &*c.func else {
                        return None;
                    };
                    match a.attr.as_str() {
                        "groupby" => Self::series_groupby_keys(expr),
                        "resample" => {
                            if let Some(on) = keyword(c, "on") {
                                return Some(vec![
                                    ast_extract::extract_string_literal(&on.value)?.to_string()
                                ]);
                            }
                            match &*a.value {
                                Expr::Call(_) => self.index_levels(&a.value),
                                _ => None,
                            }
                        }
                        _ => None,
                    }
                }
                _ => None,
            },
            Expr::Call(call) => match &*call.func {
                Expr::Attribute(a) => self.series_index(&a.value),
                _ => None,
            },
            _ => None,
        }
    }
}

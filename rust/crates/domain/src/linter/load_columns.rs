use super::*;

impl Linter {
    // Check if a base class name indicates a typedframes schema
    pub(super) fn extract_load_columns(
        &self,
        func_name: &str,
        call: &ast::ExprCall,
    ) -> (Option<Vec<String>>, LoadKind) {
        for keyword in &call.arguments.keywords {
            if let Some(value) = load_keyword_columns(func_name, keyword) {
                return value;
            }
        }

        // DataFrame({"a": [...], "b": [...]}) — a dict literal passed positionally to
        // the constructor itself already names every column as its keys, the same
        // signal `schema=` gives explicitly. Gated on func_name so this
        // never fires for read_csv/read_json/etc., whose first positional argument is
        // a path or buffer, not column data — a dict there means something else
        // entirely and inferring columns from it would be wrong.
        // `DataFrame([{...}, {...}])` -- a literal list of per-row dict literals
        // (records orientation), or `DataFrame(records)` where `records` is a name
        // this file's own pre-pass traced back to such a list built via `.append()`
        // in a loop -- see `Linter::dict_list_var_candidates`.
        if func_name == "DataFrame" {
            // A `columns=` that is not a literal list decides the columns, whatever the data names.
            if keyword(call, "columns").is_some() {
                return (None, LoadKind::File);
            }
            // `pd.DataFrame()` / `pd.DataFrame(index=..)`: no columns yet, they arrive by assignment.
            if call.arguments.args.is_empty() && keyword(call, "data").is_none() {
                return (Some(Vec::new()), LoadKind::File);
            }
            match call.arguments.args.first() {
                Some(Expr::Dict(dict)) => {
                    let keys: Vec<String> = dict
                        .items
                        .iter()
                        .filter_map(|item| item.key.as_ref())
                        .filter_map(|k| ast_extract::extract_string_literal(k))
                        .map(|s| s.to_string())
                        .collect();
                    if !keys.is_empty() {
                        return (Some(keys), LoadKind::File);
                    }
                }
                Some(Expr::List(list)) => {
                    if let Some(cols) = ast_extract::extract_records_list_columns(list)
                        .or_else(|| self.record_row_columns(list))
                    {
                        return (Some(cols), LoadKind::File);
                    }
                }
                Some(Expr::Name(name)) => {
                    if let Some(cols) = self.dict_list_var_candidates.get(name.id.as_str()) {
                        return (Some(cols.clone()), LoadKind::File);
                    }
                }
                _ => {}
            }
        }

        if !SQL_LOAD_FUNCTIONS.contains(&func_name) {
            return (None, LoadKind::File);
        }

        // chunksize= makes the call return an iterator of frames, not a single frame —
        // there is no one column set to attach to the assigned variable.
        let has_chunksize = call
            .arguments
            .keywords
            .iter()
            .any(|k| k.arg.as_ref().map(|s| s.as_str()) == Some("chunksize"));
        if has_chunksize {
            return (None, LoadKind::Sql);
        }

        // A SQLAlchemy Core statement (`pd.read_sql(select(Order.id, ...), engine)`, or
        // a `Name` bound to one) takes priority over SQL-text parsing — it isn't a
        // string at all, so `extract_sql_literal` would never match it anyway, but
        // checking first keeps the two paths clearly separate rather than relying on
        // that fallthrough.
        if let Some(cols) = self.extract_orm_select_columns(call) {
            return (Some(cols), LoadKind::Orm);
        }

        let Some(sql_text) = self.extract_sql_literal(call) else {
            return (None, LoadKind::Sql);
        };

        match sql::columns_from_select(&sql_text, self.sql_dialect) {
            sql::SqlOutcome::Columns(cols) => (Some(cols), LoadKind::Sql),
            sql::SqlOutcome::Wildcard | sql::SqlOutcome::Unparsed => (None, LoadKind::Sql),
        }
    }

    // Locate a literal SQL string passed to a SQL-shaped load call: the `sql=`/`query=`
    // keyword if present, else the first positional argument. Unwraps one layer of
    // `text(...)` / `sqlalchemy.text(...)`, the idiomatic way to pass raw SQL through a
    // SQLAlchemy engine. Returns `None` for f-strings, variables, or anything else that
    // isn't a literal — those cases fall through to the existing untracked-dataframe
    // nudge rather than being (wrongly) treated as unresolvable SQL.
    pub(super) fn extract_sql_literal(&self, call: &ast::ExprCall) -> Option<String> {
        for keyword in &call.arguments.keywords {
            if matches!(
                keyword.arg.as_ref().map(|s| s.as_str()),
                Some("sql") | Some("query")
            ) {
                return self.extract_sql_expr(&keyword.value);
            }
        }
        call.arguments
            .args
            .first()
            .and_then(|expr| self.extract_sql_expr(expr))
    }

    // Resolve a SQL-shaped argument expression to its text: a literal string, a
    // `text(...)`/`sqlalchemy.text(...)`-wrapped literal, or a `Name` bound (per
    // `string_var_candidates`) to a literal or a resolvable `.sql` file read. Deliberately
    // does NOT resolve f-strings, `.format()`, string concatenation, or any variable
    // reassigned more than once — see `StringBindingCollector` for why, and
    // `check_file_internal`'s `LoadKind::Sql` untracked-dataframe hint for how that's
    // surfaced to the user instead of silently guessing.
    pub(super) fn extract_sql_expr(&self, expr: &Expr) -> Option<String> {
        if let Some(s) = ast_extract::extract_string_literal(expr) {
            return Some(s.to_string());
        }
        if let Expr::Name(name) = expr {
            return self.string_var_candidates.get(name.id.as_str()).cloned();
        }
        if let Expr::Call(inner) = expr {
            let fn_name = match &*inner.func {
                Expr::Name(n) => n.id.as_str(),
                Expr::Attribute(a) => a.attr.as_str(),
                _ => return None,
            };
            if fn_name == "text" {
                return inner
                    .arguments
                    .args
                    .first()
                    .and_then(|expr| self.extract_sql_expr(expr));
            }
        }
        None
    }

    // Column-preserving Core statement methods: chaining any of these onto a
    // `select(...)` doesn't change which columns are projected, so `extract_select_columns`
    // sees through them to the underlying `select(...)` call. Anything else chained
    // (`.subquery()`, `.cte()`, `.union(...)`, `.add_columns(...)`, ...) isn't
    // recognized, and the whole expression is left unresolved rather than guessed at.
    pub(super) const SELECT_CHAIN_METHODS: &[&str] = &[
        "where", "filter", "join", "order_by", "limit", "offset", "group_by", "having", "distinct",
    ];

    // Locate the SQL-shaped argument (`sql=`/`query=` keyword, else first positional)
    // the same way `extract_sql_literal` does, but resolve it as a SQLAlchemy Core
    // statement's column list instead of as SQL text.
    pub(super) fn extract_orm_select_columns(&self, call: &ast::ExprCall) -> Option<Vec<String>> {
        for keyword in &call.arguments.keywords {
            if matches!(
                keyword.arg.as_ref().map(|s| s.as_str()),
                Some("sql") | Some("query")
            ) {
                return self.extract_select_columns(&keyword.value);
            }
        }
        call.arguments
            .args
            .first()
            .and_then(|expr| self.extract_select_columns(expr))
    }

    // Resolve a SQLAlchemy Core statement expression to its projected column list: a
    // `select(...)` call, a chain of `SELECT_CHAIN_METHODS` on one, or a `Name` bound
    // (via `stmt_var_candidates`) to an already-resolved one.
    pub(super) fn extract_select_columns(&self, expr: &Expr) -> Option<Vec<String>> {
        match expr {
            Expr::Name(n) => self.stmt_var_candidates.get(n.id.as_str()).cloned(),
            Expr::Call(call) => {
                let fn_name = match &*call.func {
                    Expr::Name(n) => n.id.as_str(),
                    Expr::Attribute(a) => a.attr.as_str(),
                    _ => return None,
                };
                if fn_name == "select" {
                    return self.extract_select_args(&call.arguments.args);
                }
                if Self::SELECT_CHAIN_METHODS.contains(&fn_name) {
                    if let Expr::Attribute(attr) = &*call.func {
                        return self.extract_select_columns(&attr.value);
                    }
                }
                None
            }
            _ => None,
        }
    }

    // The column list for `select(arg1, arg2, ...)`: every argument must resolve to a
    // single, unambiguous column name (see `extract_select_arg_column`), or the whole
    // select is left unresolved — never a partial list, since a silently-dropped
    // argument would understate the real projection and risk a false unknown-column
    // later on a column that's actually there. In particular a bare `select(Model)`
    // (all of a model's columns) is deliberately NOT supported: `extract_orm_columns`
    // is allowlist-based and can under-extract on an unusual declarative pattern, and
    // treating its output as "the complete column set" here would compound that.
    pub(super) fn extract_select_args(&self, args: &[Expr]) -> Option<Vec<String>> {
        if args.is_empty() {
            return None;
        }
        args.iter()
            .map(|arg| self.extract_select_arg_column(arg))
            .collect()
    }

    // A single `select(...)` argument's resulting column name: `Model.col` (an
    // attribute referencing a registered model's known column), or
    // `Model.col.label("alias")`. Anything else — a bare model, `func.count(...)`,
    // `text(...)`, a literal, a starred arg — bails the whole select rather than
    // guessing.
    fn extract_select_arg_column(&self, arg: &Expr) -> Option<String> {
        if let Expr::Call(call) = arg {
            if let Expr::Attribute(outer) = &*call.func {
                if outer.attr.as_str() == "label" {
                    self.validate_model_attribute(&outer.value)?;
                    let alias = call
                        .arguments
                        .args
                        .first()
                        .and_then(ast_extract::extract_string_literal)?;
                    return Some(alias.to_string());
                }
            }
            return None;
        }
        self.validate_model_attribute(arg)
    }

    // `Model.col` where `Model` is a registered schema (declarative model or otherwise)
    // and `col` is one of its known columns. Returns `None` — bailing the containing
    // select entirely — if `Model` isn't registered or `col` isn't one of its columns:
    // hybrid properties, synonyms, and association proxies are all legal SQLAlchemy
    // attributes with no static column mapping, so treating an unrecognized attribute
    // as an error would be wrong at least as often as it would be right.
    fn validate_model_attribute(&self, expr: &Expr) -> Option<String> {
        let Expr::Attribute(attr) = expr else {
            return None;
        };
        let Expr::Name(model) = &*attr.value else {
            return None;
        };
        let columns = self.schemas.get(model.id.as_str())?;
        let col = attr.attr.as_str();
        columns.iter().any(|c| c == col).then(|| col.to_string())
    }
}

fn load_keyword_columns(
    func_name: &str,
    keyword: &ruff_python_ast::Keyword,
) -> Option<(Option<Vec<String>>, LoadKind)> {
    let kw_name = keyword.arg.as_ref().map(|s| s.as_str());
    match kw_name {
        Some("usecols") | Some("columns") => {
            if let Some(cols) = ast_extract::extract_string_list(&keyword.value) {
                return Some((Some(cols), LoadKind::File));
            }
        }
        // Only polars' `schema=` is the complete column set. A pandas `dtype=`
        // (or `dtypes=`) mapping overrides the types of some columns and says
        // nothing about the rest.
        Some("schema") => {
            if let Expr::Dict(dict) = &keyword.value {
                let keys: Vec<String> = dict
                    .items
                    .iter()
                    .filter_map(|item| item.key.as_ref())
                    .filter_map(|k| ast_extract::extract_string_literal(k))
                    .map(|s| s.to_string())
                    .collect();
                if !keys.is_empty() {
                    return Some((Some(keys), LoadKind::File));
                }
            }
            // `pl.DataFrame(rows, schema=["a", "b"])`: the names, as strings or as
            // `(name, dtype)` pairs.
            if let (true, Expr::List(list)) = (func_name == "DataFrame", &keyword.value) {
                let names: Option<Vec<String>> = list
                    .elts
                    .iter()
                    .map(|element| match element {
                        Expr::Tuple(pair) => pair.elts.first(),
                        other => Some(other),
                    })
                    .map(|name| ast_extract::extract_string_literal(name?).map(str::to_string))
                    .collect();
                if let Some(names) = names.filter(|n| !n.is_empty()) {
                    return Some((Some(names), LoadKind::File));
                }
            }
        }
        _ => {}
    }
    None
}

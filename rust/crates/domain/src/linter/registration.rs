use super::*;

impl Linter {
    // Register `target_names` as a DataFrame materialized from a Feast retrieval.
    // `entity_cols`, when resolved, is unioned with the feature columns into one
    // concrete schema; `None` registers the result as unresolved instead, with no
    // partial credit for the feature columns alone.
    pub(super) fn register_feast_dataframe(
        &mut self,
        (cols, entity_cols): FeastColumns,
        target_names: &[String],
        var_hint: &str,
        current_line: usize,
        current_col: usize,
        errors: &mut Vec<LintError>,
    ) {
        self.dataframes_total += 1;
        match cols {
            Some(feature_cols) => {
                self.register_feast_feature_columns(
                    entity_cols,
                    target_names,
                    var_hint,
                    current_line,
                    current_col,
                    feature_cols,
                );
            }
            None => {
                self.record_untyped_site(var_hint, current_line, current_col);
                errors.push(LintError {
                    line: current_line,
                    col: current_col,
                    code: CODE_UNTRACKED_DATAFRAME.to_string(),
                    message: "columns unknown at lint time; pass a literal \
                              `features=[\"view:feature\", ...]` list to resolve the \
                              retrieved columns"
                        .to_string(),
                    severity: "warning".to_string(),
                });
            }
        }
    }

    fn register_feast_feature_columns(
        &mut self,
        entity_cols: Option<Vec<String>>,
        target_names: &[String],
        var_hint: &str,
        current_line: usize,
        current_col: usize,
        feature_cols: Vec<String>,
    ) {
        let full_cols = match &entity_cols {
            Some(entity_cols) => {
                let mut merged = entity_cols.clone();
                for c in &feature_cols {
                    if !merged.contains(c) {
                        merged.push(c.clone());
                    }
                }
                merged
            }
            None => feature_cols,
        };
        let schema_name = self.make_inferred_schema(full_cols, var_hint, current_line);
        if entity_cols.is_none() {
            self.unresolved_schemas.insert(schema_name.clone());
        }
        self.count_typed_dataframe(&schema_name, var_hint, current_line, current_col);
        for name in target_names {
            self.variables
                .insert(name.clone(), (schema_name.clone(), current_line));
        }
    }

    // If `expr` is a call to one of `SQL_PRODUCING_METHODS` (`client.query(...)`,
    // `spark.sql(...)`, `duckdb.sql(...)`/`duckdb.query(...)`), return it so its SQL
    // argument can be resolved — otherwise `None`. Kept as a separate check (rather than
    // folding straight into column extraction) so the chained-finalize dispatch below
    // can tell "not our pattern at all" (stay silent) apart from "our pattern, but the
    // SQL didn't resolve" (worth an untracked-dataframe hint) — see its call site.
    pub(super) fn register_sql_dataframe(
        &mut self,
        sql: Option<&str>,
        target_names: &[String],
        var_hint: &str,
        current_line: usize,
        current_col: usize,
        errors: &mut Vec<LintError>,
    ) {
        self.dataframes_total += 1;
        let cols = sql.and_then(
            |sql| match sql::columns_from_select(sql, self.sql_dialect) {
                sql::SqlOutcome::Columns(cols) => Some(cols),
                sql::SqlOutcome::Wildcard | sql::SqlOutcome::Unparsed => None,
            },
        );
        match cols {
            Some(cols) => {
                let schema_name = self.make_inferred_schema(cols, var_hint, current_line);
                self.count_typed_dataframe(&schema_name, var_hint, current_line, current_col);
                for name in target_names {
                    self.variables
                        .insert(name.clone(), (schema_name.clone(), current_line));
                }
            }
            None => {
                self.record_untyped_site(var_hint, current_line, current_col);
                errors.push(LintError {
                    line: current_line,
                    col: current_col,
                    code: CODE_UNTRACKED_DATAFRAME.to_string(),
                    message: "columns unknown at lint time; name the columns in the \
                              `SELECT` list instead of `SELECT *` -- an explicit list is \
                              what lets this checker (and readers) know which columns \
                              actually exist, or annotate the variable's type, e.g. \
                              `df: Annotated[pd.DataFrame, MySchema] = ...`"
                        .to_string(),
                    severity: "warning".to_string(),
                });
            }
        }
    }

    // Resolve the right-hand side of a candidate string-variable binding: a plain
    // string literal, or a recognized `.sql` file read (`open(p).read()`,
    // `Path(p).read_text()`/`pathlib.Path(p).read_text()`). Called only from
    // `StringBindingCollector` while building `string_var_candidates` — NOT from
    // `extract_sql_expr` directly, since by the time a load call is checked, any file
    // read has already been resolved once (and budget-capped) during the pre-pass.
    pub(super) fn resolve_literal_rhs(
        &self,
        expr: &Expr,
        current_file: &Path,
        reads_used: &mut u32,
    ) -> Option<String> {
        if let Some(s) = ast_extract::extract_string_literal(expr) {
            return Some(s.to_string());
        }
        if let Expr::Call(call) = expr {
            return self.resolve_file_read_call(call, current_file, reads_used);
        }
        None
    }

    // Match `open(p).read()` and `p.read_text()` (where `p` is itself a path
    // expression — `Path(p)`/`pathlib.Path(p)`, or `Path(__file__).parent / "x.sql"`
    // with no further `Path(...)` wrapper, since nobody writes
    // `Path(Path(__file__).parent / "x.sql")`), resolve the path via
    // `resolve_path_expr`, and read the file through `read_sql_file`'s safety checks.
    // The split `with open(p) as f: ... sql = f.read()` form is deliberately not
    // handled — it would need a second traced-binding namespace for file handles, and
    // the direct chained form covers the common case.
    fn resolve_file_read_call(
        &self,
        call: &ast::ExprCall,
        current_file: &Path,
        reads_used: &mut u32,
    ) -> Option<String> {
        let Expr::Attribute(attr) = &*call.func else {
            return None;
        };

        match attr.attr.as_str() {
            "read" => {
                // open(p).read()
                let Expr::Call(inner) = &*attr.value else {
                    return None;
                };
                let is_open = matches!(&*inner.func, Expr::Name(n) if n.id.as_str() == "open");
                if !is_open {
                    return None;
                }
                // Reject a binary-mode open ("rb" etc.) rather than reading raw bytes
                // as if they were UTF-8 SQL text.
                let binary_mode = inner
                    .arguments
                    .args
                    .iter()
                    .chain(inner.arguments.keywords.iter().map(|k| &k.value))
                    .any(|a| matches!(ast_extract::extract_string_literal(a), Some(m) if m.contains('b')));
                if binary_mode {
                    return None;
                }
                let path_arg = inner.arguments.args.first()?;
                let path = self.resolve_path_expr(path_arg, current_file)?;
                self.read_sql_file(&path, reads_used)
            }
            "read_text" => {
                // The receiver of `.read_text()` IS the path expression itself —
                // `Path(p).read_text()` or `(Path(__file__).parent / "x.sql").read_text()`.
                let path = self.resolve_path_expr(&attr.value, current_file)?;
                self.read_sql_file(&path, reads_used)
            }
            _ => None,
        }
    }

    // Resolve a path-shaped expression to a filesystem path: a `Path(p)`/
    // `pathlib.Path(p)` call over a string literal, or
    // `Path(__file__).parent / "literal.sql"` anchored to the file currently being
    // checked. Deliberately does NOT resolve a `Name` through `string_var_candidates` —
    // path variables and SQL-text variables share no ordering guarantee within a single
    // linear pre-pass, so doing that safely would need a fixed-point resolution loop;
    // out of scope while call sites overwhelmingly pass the path inline.
    fn resolve_path_expr(&self, expr: &Expr, current_file: &Path) -> Option<PathBuf> {
        if let Some(s) = ast_extract::extract_string_literal(expr) {
            return Some(PathBuf::from(s));
        }
        match expr {
            Expr::Call(call) if is_path_constructor(call) => call
                .arguments
                .args
                .first()
                .and_then(ast_extract::extract_string_literal)
                .map(PathBuf::from),
            Expr::BinOp(binop) => self.beside_current_file(binop, current_file),
            _ => None,
        }
    }

    // `Path(__file__).parent / "name"`.
    fn beside_current_file(&self, binop: &ast::ExprBinOp, current_file: &Path) -> Option<PathBuf> {
        if binop.op != ast::Operator::Div || !ast_extract::is_file_parent_expr(&binop.left) {
            return None;
        }
        let s = ast_extract::extract_string_literal(&binop.right)?;
        let joined = current_file.parent()?.join(s);
        // `joined` is anchored exactly the way `current_file` was passed to
        // the checker (CWD-relative, or absolute) -- never relative to
        // `project_root`. Absolutize it here so `read_sql_file` takes its
        // `is_absolute()` branch below instead of incorrectly re-joining a
        // CWD-relative path onto `project_root` (which either duplicates
        // path segments, or -- when `project_root` is itself the empty
        // PathBuf produced for a project root at the CWD -- fails outright,
        // since `fs::canonicalize("")` errors rather than meaning ".").
        Some(if joined.is_absolute() {
            joined
        } else {
            self.fs.current_dir()?.join(joined)
        })
    }

    // Matches `Path(__file__).parent` (with or without a `pathlib.` prefix on `Path`).
    pub(super) fn read_sql_file(&self, path: &Path, reads_used: &mut u32) -> Option<String> {
        if !path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("sql"))
        {
            return None;
        }
        self.read_external_file.set(true);
        let root = self.project_root.as_ref()?;
        if *reads_used >= MAX_SQL_FILE_READS_PER_FILE {
            return None;
        }

        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            root.join(path)
        };
        let canonical_root = self.fs.canonicalize(root).ok()?;
        let canonical_target = self.fs.canonicalize(&candidate).ok()?;
        if !canonical_target.starts_with(&canonical_root) {
            return None;
        }

        let metadata = self.fs.metadata(&canonical_target).ok()?;
        if !metadata.is_file || metadata.len > MAX_SQL_FILE_BYTES {
            return None;
        }

        *reads_used += 1;
        self.fs.read_to_string(&canonical_target).ok()
    }

    // Build `string_var_candidates`: names bound to a plain string literal (or a
    // resolvable `.sql` file read) exactly once anywhere in the module. Runs once, as a
    // pre-pass over the whole module before `visit_stmt`'s single top-to-bottom walk —
    // NOT incrementally during that walk. Two reasons: (1) `visit_stmt` recurses into a
    // function body at its `def` site, so a module-level constant defined *after* the
    // function using it would otherwise be invisible; (2) `self.variables` already
    // tracks names flatly with no scope stack (see its own docs), so a bare pre-pass
    // over every binding site inherits that same simplicity for free — a name assigned
    // once in each of two different functions is two bindings of one flat name, and is
    // correctly excluded as ambiguous, rather than requiring real scope resolution.
    pub(super) fn collect_literal_var_candidates(
        &self,
        body: &[Stmt],
        path: &Path,
    ) -> (HashMap<String, String>, HashMap<String, bool>) {
        let mut collector = StringBindingCollector {
            linter: self,
            current_file: path,
            reads_used: 0,
            bindings: HashMap::new(),
            binding_counts: HashMap::new(),
            bool_values: HashMap::new(),
        };
        for stmt in body {
            collector.visit_stmt(stmt);
        }
        let counts = collector.binding_counts;
        let bools = collector
            .bool_values
            .into_iter()
            .filter(|(name, _)| counts.get(name) == Some(&1))
            .collect();
        let strings = collector
            .bindings
            .into_iter()
            .filter_map(|(name, binding)| match binding {
                StringBinding::Literal(s) => Some((name, s)),
                StringBinding::Poisoned => None,
            })
            .collect();
        (strings, bools)
    }

    // Extract dropped column names from a drop() call.
    pub(super) fn make_inferred_schema(
        &mut self,
        cols: Vec<String>,
        var: &str,
        line: usize,
    ) -> String {
        // Two legs made on the same line for the same variable must not share a name: the
        // second would inherit the first's unresolved status.
        // The file is part of the name because the name travels between files through the
        // project index, where `df` at line 12 of one file must not be `df` at line 12 of
        // another.
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.file_display.hash(&mut hasher);
        let base = format!(
            "__inferred_{}_{:08x}_at_{}",
            var,
            hasher.finish() as u32,
            line
        );
        let mut name = base.clone();
        let mut suffix = 2;
        while self.schemas.contains_key(&name) {
            name = format!("{base}_{suffix}");
            suffix += 1;
        }
        self.schemas.insert(name.clone(), cols);
        name
    }

    // Resolve a `self.functions`/`class_methods`/cross-file `class_methods`
    // entry to an actual schema name usable in `self.variables`. Most entries already
    // ARE a real schema name and pass through unchanged; `OPEN_FRAME_MARKER` is the one
    // exception (a bare `pd.DataFrame`/`pl.DataFrame` return with no attached Schema —
    // see its doc comment), which this synthesizes into a fresh, unresolved, empty
    // schema, the same way `register_feast_dataframe` does for a Feast retrieval whose
    // entity_df didn't resolve. Centralized here because every call-resolution site
    // that consults one of those maps (module-level bare calls, same-file
    // `self.<attr>.<method>()`, and the upcoming cross-file case) needs the exact same
    // marker-or-real-name branch.
    pub(super) fn resolve_open_or_named_schema(
        &mut self,
        marker_or_schema: &str,
        var_hint: &str,
        line: usize,
    ) -> String {
        if marker_or_schema == OPEN_FRAME_MARKER {
            let name = self.make_inferred_schema(Vec::new(), var_hint, line);
            self.unresolved_schemas.insert(name.clone());
            name
        } else {
            marker_or_schema.to_string()
        }
    }
}

// The definition line an inferred schema's name carries: `__inferred_<var>_<hash>_at_<line>`,
// with `_<n>` appended when two share a name.
pub(super) fn inferred_schema_line(name: &str) -> Option<usize> {
    let (_, tail) = name.strip_prefix("__inferred_")?.rsplit_once("_at_")?;
    tail.split('_').next()?.parse().ok()
}

// `Path(...)` or `pathlib.Path(...)`.
fn is_path_constructor(call: &ast::ExprCall) -> bool {
    match &*call.func {
        Expr::Name(n) => n.id.as_str() == "Path",
        Expr::Attribute(a) => a.attr.as_str() == "Path",
        _ => false,
    }
}
